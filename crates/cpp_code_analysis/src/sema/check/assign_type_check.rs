//! **`assign-type-check`**: a value that does not convert to the type it is initialised as.
//!
//! ```cpp
//! int  count = "three";      // a string where an int is declared
//! bool ready = 3.5;          // no conversion from a floating-point value to `bool` in this language
//! ```
//!
//! # Only a declaration with a written type and an initialiser
//!
//! ```text
//!   reported      `T x = expr;` and `T x{expr};` where `expr` does not convert to `T`, and `T` is written here
//!   not reported  `auto x = expr;`     a deduction question, not a conversion one
//!   not reported  `x = expr;`          an assignment to an existing object — the object's type is a question about
//!                                      scopes rather than about this declaration, and the check that will answer it
//!                                      needs the model's own resolution rather than a spelling
//!   not reported  `T x;`               nothing is being converted
//!   not reported  a parameter         the argument's conversion is `param-type-check`'s question
//! ```
//!
//! The second and third lines are the honest boundary of this check rather than a wish list: an assignment needs to
//! resolve the object on the left, and that is [`Session::type_at`](crate::Session::type_at)'s question. When that
//! is answered for assignments the rule extends to them; guessing at it here would be a second, worse answer.
//!
//! # The pairing, and the cost that was measured for the wrong one
//!
//! Each initialiser is paired with the declarator written **beside it**, in one walk over the tree: the walk
//! collects the initialisers, and the name written immediately before one is the name being initialised. The check
//! this replaces did it the other way round — for every declaration, search the tree for its initialiser — and its
//! documentation records what that cost: **6 430 ms over twenty MSVC headers, where the other checks together cost
//! 396 ms**, 94% of the layer, paid per keystroke. One walk is what makes this version linear in the file.
//!
//! # `Known::No` and nothing else
//!
//! The comparison is [`Type::convertible_to`](crate::sema::types::Type::convertible_to), the crate's single answer,
//! and it is `Known`-valued: a class with a constructor taking the argument, a template, a type from a header
//! nobody read all answer `Unknown`, and **`Unknown` reports nothing**.

use std::collections::HashMap;

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`assign-type-check`** — see the module documentation.
pub struct AssignTypeCheck;

impl Check for AssignTypeCheck {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AssignTypeCheck];

    fn check(context: &mut CheckContext<'_>) {
        // **The written type of every variable this file declares**, by name. Built first so that the walk below is
        // one pass over the tree and one lookup per initialiser — see the module documentation for what the
        // opposite order cost.
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

        for initializer in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        {
            // **The expression being converted**, which is what the initialiser holds. An empty initialiser is a
            // file being typed at.
            // The punctuation the initialiser is written with is not the expression: `= expr`, `{expr}` and
            // `(expr)` all leave one child that is the value.
            let expression = initializer.children().find(|child| {
                !matches!(
                    child.text().to_string().trim(),
                    "=" | "{" | "}" | "(" | ")" | ","
                )
            });
            let Some(expression) = expression else {
                continue;
            };

            // **The name written beside it**: the last identifier before the `=` or the `{`, which is the declarator
            // this initialiser belongs to.
            let before_the_initializer = context.source.get(..initializer.text_range().start().into());
            let Some(before_the_initializer) = before_the_initializer else {
                continue;
            };
            let Some(name) = last_identifier_of(before_the_initializer) else {
                continue;
            };
            let Some(written) = declared.get(name.as_str()).copied() else {
                continue;
            };

            let crate::Known::Yes((from, _)) = context.type_of(&expression) else {
                continue;
            };
            if !from
                .convertible_to(&crate::sema::types::parse_type_spelling(written))
                .is_definitely_absent()
            {
                continue;
            }

            context.add(
                DiagnosticCode::AssignTypeCheck,
                cpp_parser::source_range(expression.text_range()),
                name.clone(),
                format!(
                    "`{}` is initialised with a value that does not convert to `{written}`",
                    name
                ),
            );
        }
    }
}

/// The last identifier written in a piece of text.
///
/// Text rather than a node walk on purpose: what has to be found is "the declarator this initialiser belongs to",
/// and the declarator is whatever identifier the reader last wrote before the `=`. A node walk would have to
/// decide which recovered node that is, and the recovery around a declarator is the least reliable shape in the
/// tree.
fn last_identifier_of(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
            continue;
        }
        if !current.is_empty() {
            if !current.chars().next().is_some_and(|first| first.is_ascii_digit()) {
                last = Some(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && !current.chars().next().is_some_and(|first| first.is_ascii_digit()) {
        last = Some(current);
    }
    last
}

