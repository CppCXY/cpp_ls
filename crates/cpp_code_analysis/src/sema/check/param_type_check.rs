//! **`param-type-check`**: an argument whose type does not convert to the parameter it is passed for.
//!
//! ```cpp
//! void takes_a_number(int);
//!
//! takes_a_number(1);        // fine
//! takes_a_number("one");    // a string where an int is declared
//! ```
//!
//! # One callee, declared here — and an overload set, judged one candidate at a time
//!
//! Two restrictions, and both are about not inventing problems:
//!
//! ```text
//!   one function of that name in this file       a name that denotes an **overload set** is a call whose meaning
//!                                               is the language's resolution, and a check with a rule of its own
//!                                               would be wrong about every overload it did not pick
//!   the parameters come from **this file's tree**    see [`super::parameter_spellings_of`] for what reading
//!                                               `DeclFact::parameters` instead cost — 119 findings on 200 MSVC headers
//! ```
//!
//! **A member call is the exception, and it is the ordinary case in the standard library.** A member's parameters
//! come from the index (`members_of`, with the class's arguments substituted), and the library writes every
//! mutator twice — `push_back(const _Ty&)` and `push_back(_Ty&&)` — so "several declarations, say nothing" was
//! silence on nearly every member call there is. What replaces choosing is the rule this check has always had:
//! a finding needs **every** candidate to disagree. One candidate that takes the argument, or one this layer
//! cannot compare, and nothing is said.
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
                let parameters = member_parameter_lists(context, &access);
                if !parameters.is_empty() {
                    check_the_arguments(context, &call, &access.member, &parameters);
                }
                continue;
            }

            // **Exactly one declaration, or nothing is said.**
            let Some(Some(parameters)) = by_name.get(name.as_str()) else {
                continue;
            };
            check_the_arguments(context, &call, &name, std::slice::from_ref(parameters));
        }
    }
}

/// **Every parameter list a member of that name is declared with** — with the class's arguments substituted by
/// `members_of`.
///
/// A list rather than one, because **an overload set is the ordinary case**: MSVC's `std::vector` writes
/// `void push_back(const _Ty& _Val)` *and* `void push_back(_Ty&& _Val)`, and the earlier version of this function
/// answered "several members, so nothing to say" — which is how `anotherVec.push_back("123131")` on a
/// `std::vector<int>` was never reported. What the set needs is not a choice between the candidates (that is
/// overload resolution, and a check with a rule of its own would be wrong about every candidate it did not pick)
/// but the rule [`check_the_arguments`] applies: a finding needs **every** candidate to disagree.
///
/// `parameters_in` strips each parameter's name, so what comes back is comparable with a type spelling. A member
/// with no parameter list — a field — is not a candidate: it is not a call.
fn member_parameter_lists(
    context: &CheckContext<'_>,
    access: &crate::sema::resolve::MemberAccess,
) -> Vec<Vec<String>> {
    let crate::Known::Yes((a_type, _)) = context.type_of(&access.object) else {
        return Vec::new();
    };
    let crate::Known::Yes(list) = context.model.members_of(&a_type.to_string()) else {
        return Vec::new();
    };
    list.members
        .iter()
        .filter(|member| member.fact.name == access.member)
        .filter_map(|member| member.fact.parameter_list.as_deref())
        .map(|written| crate::sema::overload::parameters_in(written).0)
        .collect()
}

/// **One call's arguments against every declaration the name has** — the comparison both paths make.
///
/// # The rule for more than one declaration
///
/// A finding needs **every** candidate to disagree, which is the rule this check has always been documented with
/// and the one the member path had no way to apply: one candidate whose parameter takes the argument (`Yes`), or
/// one this layer cannot compare (`Unknown`), makes the call something a check must not describe. `No`, `No`, `No`
/// is the only shape that reports — and the message quotes the **first** candidate, because the reader's question
/// is what the declaration they are looking at says, not a list.
fn check_the_arguments(
    context: &mut CheckContext<'_>,
    call: &cpp_parser::CppSyntaxNode,
    name: &str,
    candidates: &[Vec<String>],
) {
    // **The typed accessor, not `children()`.** A simple argument is a **token** in this grammar — a string literal is
    // one — so a walk over the argument list's child *nodes* comes out empty and this comparison never ran. `get_args`
    // is what `argument-count` uses for the same reason.
    let Some(call) = cpp_parser::CppCallExpr::cast(call.clone()) else {
        return;
    };

    for (at, argument) in call.get_args().iter().enumerate() {
        let crate::Known::Yes((from, _)) = context.type_of(argument.syntax()) else {
            continue;
        };

        // **The candidates that can take an argument in this position.** A list with fewer parameters is a call
        // the arity check owns, not a disagreement about this one.
        let mut disagreed_with: Option<&String> = None;
        let mut every_candidate_disagrees = true;
        for parameters in candidates {
            let Some(parameter) = parameters.get(at) else {
                continue;
            };
            let target = crate::sema::types::parse_type_spelling(parameter);
            match from.convertible_to(&target) {
                crate::Known::No => {
                    disagreed_with.get_or_insert(parameter);
                }
                // `Yes` is fine, and `Unknown` is not this layer's to guess about. Either way **this** candidate
                // could take the argument, and one that could is enough for silence.
                crate::Known::Yes(()) | crate::Known::Unknown(_) => {
                    every_candidate_disagrees = false;
                    break;
                }
            }
        }

        let Some(parameter) = disagreed_with else {
            continue;
        };
        if !every_candidate_disagrees {
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