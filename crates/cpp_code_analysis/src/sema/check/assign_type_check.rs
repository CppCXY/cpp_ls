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

use cpp_parser::{CppAstNode, CppSyntaxKind};

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
            // Nothing to judge: no variable in this file has a written type. The ordinary answer for a file that
            // only declares functions, and the reason this check costs nothing on one.
            return;
        }

        // **The declarations**: `T x = expr;`. The value is the initialiser's own expression, and the name is the
        // declarator written beside it.
        // **This file's own initialisers** - see CheckContext::own_syntax_nodes.
        for initializer in context
            .own::<cpp_parser::CppInitializer>()
            .map(|initializer| initializer.syntax().clone())
            .collect::<Vec<_>>()
        {
            // The punctuation is not the expression: `= expr`, `{expr}` and `(expr)` each leave one child that is
            // the value.
            let Some(expression) = initializer
                .children()
                .find(|child| !is_punctuation(&child.text().to_string()))
            else {
                continue;
            };

            // **The name written beside it, from the tree**: the initialiser's parent is the declarator it belongs
            // to, and the name is the last identifier inside that. This was a text scan — "the last identifier
            // before the `=`" — which is a decision made by looking at characters, and the check layer's own rule is
            // that a decision comes from nodes and tokens while text is for messages. It is also what produced the
            // false positive in `assignment_in_condition`: reading a language by its punctuation is how a `<` in a
            // condition became a bracket.
            let Some(declarator) = initializer
                .parent()
                .and_then(|holder| {
                    holder.children().find(|child| {
                        CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator
                    })
                })
                .or_else(|| initializer.parent())
            else {
                continue;
            };
            let Some(name) = super::spelling_of(&declarator) else {
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
    let target = crate::sema::types::parse_type_spelling(written);
    if from.convertible_to(&target).is_definitely_absent() {
        report(context, expression, name, how, written);
        return;
    }

    // **The class case, which the conversion relation cannot answer.** `convertible_to` lives in the type engine and
    // cannot see a declaration, so for any class target it answers `Unknown` — which is the honest answer to "can
    // this convert", and the *wrong* answer to "does this class have a constructor that could take this". Measured:
    // with the header indexed and `MyString` a known class with only a default constructor,
    // `MyString line = 1231;` produced nothing at all.
    //
    // So the question is asked where the class's **definition** is readable: in this file's own tree, which is the
    // scope this analysis checks. A class from a header is left alone — its constructors are in another file, and a
    // constructor's parameter list is not in the member list this layer can see (see `parameter_spellings_of` for
    // what that field really is).
    if a_class_in_this_file_cannot_be_built_from(context, &target, &from, expression) {
        report(context, expression, name, how, written);
    }
}

/// **Does this file define the class, and can nothing in it build the class from this value?**
///
/// The narrow question the conversion relation cannot answer, restricted three ways so that it can only fire when
/// the answer is in front of it:
///
/// ```text
///   the target is a class **defined in this file**   a header's class has its constructors elsewhere
///   it declares **no constructor at all**            any constructor might take the value, and deciding which is
///                                                    overload resolution rather than a walk over a tree
///   it declares **no conversion operator**           `operator int()` is a way to build one that has no parameter
///                                                    list to compare against
///   the value is a **literal or an arithmetic type** a class-to-class initialisation is a question about two
///                                                    declarations, and only one of them is here
/// ```
fn a_class_in_this_file_cannot_be_built_from(
    context: &CheckContext<'_>,
    target: &crate::sema::types::Type,
    from: &crate::sema::types::Type,
    expression: &cpp_parser::CppSyntaxNode,
) -> bool {
    let Some(class) = target.class_name() else {
        return false;
    };
    // The value has to be a literal or an arithmetic type — see the list above.
    let text = expression.text().to_string();
    let text = text.trim();
    let a_literal = text.starts_with('"') || text.starts_with('\'') || text.starts_with(|c: char| c.is_ascii_digit());
    if !from.is_arithmetic() && !a_literal {
        return false;
    }

    // The class's definition **in this file**.
    let Some(definition) = definition_of_a_class(context, class) else {
        return false;
    };

    // **A constructor is a member named after its class; a conversion operator is one whose name begins with
    // `operator`.** Either of them answers "can this class be built from this value" in a way this walk cannot
    // decide, so either of them is enough for the rule to have nothing to say. Asked of the tree — this was a text
    // test that looked for a `{` and a name in a node's text, and the layer's own rule is that a decision comes
    // from nodes.
    let has_one = definition
        .descendants()
        .filter(|node| {
            CppSyntaxKind::from(node.kind()) != CppSyntaxKind::ParameterList
                && (super::is_a_function_definition(node)
                    || node.children().any(|child| {
                        CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList
                    }))
        })
        .any(|node| {
            super::name_declared_by(&node)
                .is_some_and(|name| name == class || name.starts_with("operator"))
        });

    !has_one
}

/// **The definition of the class with this name, when this file holds one.**
///
/// A `ClassDef` node — that is what the grammar builds for `class`, `struct` and `union` alike, with the keyword as
/// a token inside it — whose own `NameExpr` spells the name asked about.
///
/// This was a text test: *does the text start with `class `, hold a `{`, and contain the name* — and it returned
/// `None` on `struct MyString { MyString(); int size; };`, whose definition is right there. Measured at the time:
/// the probe printed ``no definition in this file`` while the class was in the file, which is what sent the search
/// back to the tree.
fn definition_of_a_class(
    context: &CheckContext<'_>,
    class: &str,
) -> Option<cpp_parser::CppSyntaxNode> {
    context
        .tree()
        .descendants()
        .filter(super::is_a_class_definition)
        .find(|node| {
            node.children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)
                .and_then(|name| super::spelling_of(&name))
                .is_some_and(|name| name == class || name.ends_with(&format!("::{class}")))
        })
}

/// Put the finding where every other one goes.
fn report(
    context: &mut CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    name: &str,
    how: &str,
    written: &str,
) {
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






