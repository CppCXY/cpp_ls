//! **`param-type-check`**: an argument whose type does not convert to the parameter it is passed for.
//!
//! ```cpp
//! void takes_a_number(int);
//!
//! takes_a_number(1);        // fine
//! takes_a_number("one");    // a string where an int is declared
//! ```
//!
//! # One callee, declared here, or nothing is said
//!
//! Two restrictions, and both are about not inventing problems:
//!
//! ```text
//!   one function of that name in this file    a name that denotes an **overload set** is a call whose meaning is
//!                                             the language's resolution, and a check with a rule of its own would
//!                                             be wrong about every overload it did not pick. The check this one
//!                                             replaces reported only when *every* candidate disagreed, and its
//!                                             documentation says why: with two candidates, one is usually right
//!   the parameters come from **this file's tree**   see [`super::parameter_spellings_of`] for what reading
//!                                             `DeclFact::parameters` instead cost — 119 findings on 200 MSVC headers
//! ```
//!
//! # `Known::No` and nothing else
//!
//! [`Type::convertible_to`](crate::sema::types::Type::convertible_to) is the crate's single answer to "can a value
//! of this type be used where that one is declared", and it is `Known`-valued: a class with a conversion
//! constructor, a template, a type from a header nobody read all answer `Unknown`, and **`Unknown` reports
//! nothing**. A conversion this layer cannot see is not a conversion the program does not have.

use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

/// **`param-type-check`** — see the module documentation.
pub struct ParamTypeCheck;

impl Check for ParamTypeCheck {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ParamTypeCheck];

    fn check(context: &mut CheckContext<'_>) {
        // **The functions of this file that are declared exactly once**, by name. A name declared twice is an
        // overload set, and an overload set is not this check's question — see the module documentation. The
        // `Option` is the whole of that rule: `None` means "more than one declaration", and the call loop below
        // skips those names without asking anything else.
        let mut by_name: std::collections::HashMap<String, Option<Vec<String>>> =
            std::collections::HashMap::new();
        for function in context
            .tree()
            .descendants()
            .filter(|node| super::is_a_function_definition(node))
        {
            let Some(name) = super::name_declared_by(&function) else {
                continue;
            };
            let parameters = super::parameter_spellings_of(&function);
            if by_name.contains_key(&name) {
                // **A second declaration of one name is an overload set** — see the module documentation. `None`
                // is the mark, and the call loop below skips those names without asking anything else.
                by_name.insert(name, None);
            } else {
                by_name.insert(name, Some(parameters));
            }
        }

        for call in context
            .tree()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
        {
            let Some(callee) = call.children().next() else {
                continue;
            };
            let Some(name) = super::spelling_of(&callee) else {
                continue;
            };
            // **Exactly one declaration, or nothing is said.**
            let Some(Some(parameters)) = by_name.get(name.as_str()) else {
                continue;
            };
            let Some(arguments) = call
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList)
            else {
                continue;
            };

            for (argument, parameter) in arguments.children().zip(parameters.iter()) {
                let crate::Known::Yes((from, _)) = context.type_of(&argument) else {
                    continue;
                };
                let target = crate::sema::types::parse_type_spelling(parameter);
                if !matches!(from.convertible_to(&target), crate::Known::No) {
                    // `Yes` is fine, and `Unknown` is not this layer's to guess about.
                    continue;
                }
                context.add(
                    DiagnosticCode::ParamTypeCheck,
                    cpp_parser::source_range(argument.text_range()),
                    name.clone(),
                    format!(
                        "this passes `{}` where `{parameter}` is declared for `{name}`",
                        context.source
                            .get(argument.text_range().start().into()..argument.text_range().end().into())
                            .unwrap_or_default()
                            .trim()
                    ),
                );
            }
        }
    }
}




