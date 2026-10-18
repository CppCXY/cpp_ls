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

use cpp_parser::CppAstNode;

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
        // **This file's own functions** - see CheckContext::own_syntax_nodes.
        for function in context
            .own_syntax_nodes()
            .filter(|node| super::is_a_function_definition(node))
            .collect::<Vec<_>>()
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

        // **This file's own calls**, typed, over this file's own text — see `CheckContext::own`. Collected before
        // the loop because a finding needs `&mut context` while the iterator holds `&context`.
        for call in context.own::<cpp_parser::CppCallExpr>().collect::<Vec<_>>() {
            let call = call.syntax().clone();
            let Some(callee) = call.children().next() else {
                continue;
            };
            let Some(name) = super::spelling_of(&callee) else {
                continue;
            };

            // **A member call is checked against the member.** `members_of` has already substituted the class's
            // arguments into its members — `push_back(T value)` on a `std::vector<int>` is `(int value)` — and
            // `parameters_in` strips the parameter name, so the comparison is the same one the free-function path
            // makes. Without this, a member call was skipped by the `by_name` lookup below: a member of a class is a
            // **declaration**, and that table is built from this file's function **definitions**.
            if let Some(access) = super::member_access_above(&callee) {
                if let Some(parameters) = member_parameters(context, &access) {
                    check_the_arguments(context, &call, &access.member, &parameters);
                }
                continue;
            }

            // **Exactly one declaration, or nothing is said.**
            let Some(Some(parameters)) = by_name.get(name.as_str()) else {
                continue;
            };
            check_the_arguments(context, &call, &name, parameters);
        }
    }
}

/// **The member a call names, or `None`** — with the class's arguments already substituted by `members_of`.
///
/// One member is this check's question; several are an overload set, which it declines to resolve (module
/// documentation). `parameters_in` strips each parameter's name, so what comes back is comparable with a type spelling.
fn member_parameters(
    context: &CheckContext<'_>,
    access: &crate::sema::resolve::MemberAccess,
) -> Option<Vec<String>> {
    let crate::Known::Yes((a_type, _)) = context.type_of(&access.object) else {
        return None;
    };
    let crate::Known::Yes(list) = context.model.members_of(&a_type.to_string()) else {
        return None;
    };
    let mut found: Vec<Vec<String>> = Vec::new();
    for member in &list.members {
        if member.fact.name != access.member {
            continue;
        }
        let Some(written) = member.fact.parameter_list.as_deref() else {
            continue;
        };
        let (parameters, _variadic) = crate::sema::overload::parameters_in(written);
        if !parameters.is_empty() {
            found.push(parameters);
        }
    }

    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// **One call's arguments against one declaration's parameter spellings** — the comparison both paths make.
fn check_the_arguments(
    context: &mut CheckContext<'_>,
    call: &cpp_parser::CppSyntaxNode,
    name: &str,
    parameters: &[String],
) {
    // **The typed accessor, not `children()`.** A simple argument is a **token** in this grammar — a string literal is
    // one — so a walk over the argument list's child *nodes* comes out empty and this comparison never ran. `get_args`
    // is what `argument-count` uses for the same reason.
    let Some(call) = cpp_parser::CppCallExpr::cast(call.clone()) else {
        return;
    };

    for (argument, parameter) in call.get_args().iter().zip(parameters.iter()) {
        let crate::Known::Yes((from, _)) = context.type_of(argument.syntax()) else {
            continue;
        };

        let target = crate::sema::types::parse_type_spelling(parameter);
        if !matches!(from.convertible_to(&target), crate::Known::No) {
            // `Yes` is fine, and `Unknown` is not this layer's to guess about.
            continue;
        }
        context.add(
            DiagnosticCode::ParamTypeCheck,
            cpp_parser::source_range(argument.syntax().text_range()),
            name.to_string(),
            format!(
                "this passes `{}` where `{parameter}` is declared for `{name}`",
                context
                    .source
                    .get(
                        argument.syntax().text_range().start().into()
                            ..argument.syntax().text_range().end().into()
                    )
                    .unwrap_or_default()
                    .trim()
            ),
        );
    }
}