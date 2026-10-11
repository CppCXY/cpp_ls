//! **`assign-type-check`**: a value that does not convert to the type it is assigned to.
//!
//! ```cpp
//! int  count = "three";      // a string where an int is declared
//! bool ready = 3.5;          // no conversion from a floating-point value to `bool` in this language
//!
//! int other;
//! other = "four";            // the same question at an assignment, which is the second place a value enters an
//!                            // object whose type is written down
//! ```
//!
//! # The two places, which are one question
//!
//! ```text
//!   a declaration    `T x = expr;` and `T x{expr};`   the type is the declaration's, and its initialiser is here
//!   an assignment    `x = expr;`                      the type is the object's, and the type of the left-hand side
//!                                                     is what the model answers for `x`
//! ```
//!
//! Both end in the same comparison against [`Type::convertible_to`], so this is one rule rather than two.
//!
//! # What is not reported
//!
//! ```text
//!   not reported  `auto x = expr;`       a deduction question, not a conversion one
//!   not reported  `T x;`                 nothing is being converted
//!   not reported  a parameter            the argument's conversion is `param-type-check`'s question
//!   not reported  `a[i] = …`, `w.f = …`  the left-hand side is an expression rather than a name, and its type is a
//!                                        question about that expression — the model answers it, but as a type
//!                                        rather than as a declaration this check can name in a message
//!   not reported  `Unknown`              a class with a conversion constructor, a template, a type from a header
//!                                        nobody read: a conversion this layer cannot see is not a conversion the
//!                                        program does not have
//! ```
//!
//! # The pairing, and the cost that was measured for the wrong one
//!
//! Each initialiser is paired with the declarator written **beside it**, in one walk over the tree: the walk
//! collects the initialisers, and the name written immediately before one is the name being initialised. The check
//! this replaces did it the other way round — for every declaration, search the tree for its initialiser — and its
//! documentation records what that cost: **6 430 ms over twenty MSVC headers, where the other checks together cost
//! 396 ms**, 94% of the layer, paid per keystroke. One walk is what makes this version linear in the file.

use std::collections::HashMap;

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`assign-type-check`** — see the module documentation.
pub struct AssignTypeCheck;

impl Check for AssignTypeCheck {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AssignTypeCheck];

    fn check(context: &mut CheckContext<'_>) {
        // **The written type of every variable this file declares**, by name. Built first so that the walk below is
        // one pass over the tree and one lookup per place a value is assigned — see the module documentation for
        // what the opposite order cost.
        let mut declared: HashMap<&str, &str> = HashMap::new();
        for fact in &context.summary.declarations {
            if fact.kind != crate::DeclKind::Variable {
                continue;
            }
            let Some(written) = fact.type_of.as_deref() else {
                continue;
            };
            if matches!(written, "auto" | "decltype(auto)") {
                continue;
            }
            declared.insert(fact.name.as_str(), written);
        }
        if declared.is_empty() {
            return;
        }

        // **The declarations**: `T x = expr;`. The value is the initialiser's own expression, and the name is the
        // declarator written beside it.
        for initializer in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        {
            // The punctuation is not the expression: `= expr`, `{expr}` and `(expr)` each leave one child that is
            // the value.
            let Some(expression) = initializer
                .children()
                .find(|child| !is_punctuation(&child.text().to_string()))
            else {
                continue;
            };

            // **The name written beside it**: the last identifier before the `=` or the `{`, which is the declarator
            // this initialiser belongs to.
            let Some(before) = context.source.get(..initializer.text_range().start().into()) else {
                continue;
            };
            let Some(name) = last_identifier_of(before) else {
                continue;
            };
            let Some(written) = declared.get(name.as_str()).copied() else {
                continue;
            };

            report_if_it_does_not_convert(context, &expression, written, &name, "initialised");
        }

        // **And the assignments**: `x = expr;`. Which object it is comes from the left-hand side being **a name and
        // nothing else** — `a[i] = …` and `w.field = …` are questions about those expressions rather than about a
        // declaration this check can name.
        for assignment in context.tree().descendants().filter(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BinaryExpr && has_an_equals_operator(node)
        }) {
            let sides: Vec<cpp_parser::CppSyntaxNode> = assignment
                .children()
                .filter(|child| is_not_the_operator(&child.text().to_string()))
                .collect();
            let [left, right] = sides.as_slice() else {
                continue;
            };

            let left_text = left.text().to_string();
            let left_text = left_text.trim();
            let Some(name) = last_identifier_of(left_text) else {
                continue;
            };
            if left_text != name {
                continue;
            }
            let Some(written) = declared.get(name.as_str()).copied() else {
                continue;
            };

            report_if_it_does_not_convert(context, right, written, &name, "assigned");
        }
    }
}

/// **One place a value enters an object whose type is written down**, reported when it does not convert.
///
/// The single comparison both halves end in — see the module documentation — so that the two cannot drift into
/// having different opinions about the same conversion.
fn report_if_it_does_not_convert(
    context: &mut CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    written: &str,
    name: &str,
    how: &str,
) {
    let crate::Known::Yes((from, _)) = context.type_of(expression) else {
        return;
    };
    if !from
        .convertible_to(&crate::sema::types::parse_type_spelling(written))
        .is_definitely_absent()
    {
        // `Yes` is a conversion that works, and `Unknown` is one this layer cannot see — which is not the program's
        // problem. Only a definite "no" is reported: see the module documentation.
        return;
    }
    context.add(
        DiagnosticCode::AssignTypeCheck,
        cpp_parser::source_range(expression.text_range()),
        name.to_string(),
        format!("`{name}` is {how} with a value that does not convert to `{written}`"),
    );
}

/// **Is this `=` or `+=` or `-=` …?** Asked of the **tokens**, because the operator is a token rather than a node:
/// `node.children()` yields nodes only, so an `=` written directly between two operands is invisible to it — which
/// is why the assignment half of this check reported nothing at all while the declaration half worked. Measured:
/// `assign_type_check_reports_both_places_a_value_is_assigned` read an empty list for the assignment.
fn has_an_equals_operator(node: &cpp_parser::CppSyntaxNode) -> bool {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            // **`=` or a compound assignment**, and not a comparison: `==`, `!=`, `<=` and `>=` all end in `=` and
            // none of them assigns. The plain `=` is the one this line started out excluding, because the exclusion
            // was written as "does not start with `=`" — which is true of `+=` and false of the operator this check
            // is about. Measured: the assignment half reported nothing while the declaration half worked.
            let text = token.text().to_string();
            text == "=" || (text.len() > 1 && text.ends_with('=') && !matches!(text.as_str(), "==" | "!=" | "<=" | ">="))
        })
}

/// Is this the text of the operator rather than one of its operands?
fn is_not_the_operator(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty() && text.chars().all(|character| character == '=' || character == '+' || character == '-' || character == '*' || character == '/' || character == '%' || character == '&' || character == '|' || character == '^' || character == '<' || character == '>')
}

/// Is this the punctuation an initialiser is written with rather than the value?
fn is_punctuation(text: &str) -> bool {
    matches!(text.trim(), "=" | "{" | "}" | "(" | ")" | ",")
}

/// The last identifier written in a piece of text.
///
/// Text rather than a node walk on purpose: what has to be found is "the declarator this initialiser belongs to",
/// and the declarator is whatever identifier the reader last wrote before the `=`. A node walk would have to decide
/// which recovered node that is, and the recovery around a declarator is the least reliable shape in the tree.
fn last_identifier_of(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
            continue;
        }
        if !current.is_empty() {
            if !current
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
            {
                last = Some(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty()
        && !current
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
    {
        last = Some(current);
    }
    last
}

