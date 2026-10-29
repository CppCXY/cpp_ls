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
            .filter(super::is_a_function_definition)
            .collect::<Vec<_>>()
        {
            let Some(name) = super::name_declared_by(&function) else {
                continue;
            };
            // **The name is not part of the type.** `parameter_spellings_of` answers what the file *wrote* — `int
            // width` — and a comparison against a type has to see `int`; `a_type_without_its_name` is the reader the
            // rest of the analysis already uses for exactly this (`sema::overload::parameters_in` calls it). Without the
            // strip, the same call was reported when the parameter was unnamed (`void resize(int)`) and silent when it
            // was named — which is the shape every real codebase writes.
            let parameters: Vec<String> = super::parameter_spellings_of(&function)
                .iter()
                .map(|spelling| crate::sema::overload::a_type_without_its_name(spelling))
                .collect();
            match by_name.entry(name) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    // **A second declaration of one name is an overload set** — see the module documentation. `None`
                    // is the mark, and the call loop below skips those names without asking anything else.
                    e.insert(None);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Some(parameters));
                }
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

            // **Exactly one declaration, or nothing is said.** The table above holds this file's function *definitions* — the
            // rule that keeps an overload set out of a comparison about one parameter list. But a function declared once in a
            // header and called here has exactly one parameter list too, and that is the most common shape in a project, so a
            // name the table does not hold is asked of the index before this check gives up.
            let parameters: Vec<String> = match by_name.get(name.as_str()) {
                Some(Some(parameters)) => parameters.clone(),
                // A name declared twice in this file is an overload set: not this check's question.
                Some(None) => continue,
                None => match the_index_declares_one(context, name.as_str()) {
                    Some(parameters) => parameters,
                    None => continue,
                },
            };
            check_the_arguments(context, &call, &name, std::slice::from_ref(&parameters));
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
                crate::Known::Yes(()) => {
                    every_candidate_disagrees = false;
                    break;
                }
                // **The class case, inside the `Unknown` arm.** `convertible_to` answers `Unknown` for every class target —
                // it lives in the type algebra and never sees a constructor — so a parameter written `const std::string&` was
                // silent whatever the argument was. The question goes to the index instead, and **only a definite refusal
                // counts**: a class nobody has read, or a template parameter, still answers `None` and stays quiet.
                // **The class case, through the shared facility** — see `sema::convert`. It normalises the spelling (a
                // `const std::string&` parameter is `Reference { to: Named(..) }`, and `class_name()` answers `None` for a
                // reference) and asks the index only for the class question, so a class nobody has read stays quiet.
                crate::Known::Unknown(_) => {
                    let conversion = crate::sema::convert::conversion(&from, parameter, |class| {
                        super::a_class_can_be_built_from(context, class, &from)
                    });
                    if !conversion.is_refused() {
                        every_candidate_disagrees = false;
                        break;
                    }
                    disagreed_with.get_or_insert(parameter);
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

/// **The one parameter list the index has for a name**, with its parameter names stripped — the fallback for a function
/// declared in a header and called here.
///
/// `None` when the index knows no such function, knows several (an overload set, which this check declines), or when the
/// declaration has no parameter list to compare against.
fn the_index_declares_one(context: &CheckContext<'_>, name: &str) -> Option<Vec<String>> {
    // **The file's own summary first, then the index.** The summary holds what this file declares; `definitions` answers for
    // the rest of the project. A name declared once in either is one parameter list; several is an overload set, which this
    // check declines to resolve.
    let mut written: Vec<String> = context
        .summary
        .declarations
        .iter()
        .filter(|fact| fact.kind == crate::DeclKind::Function && fact.name == name)
        .filter_map(|fact| fact.parameter_list.clone())
        .collect();
    if written.is_empty()
        && let crate::Known::Yes(definitions) = context.index().definitions(name, context.path)
    {
        written = definitions
            .found
            .iter()
            .filter(|definition| definition.fact.kind == crate::DeclKind::Function)
            .filter_map(|definition| definition.fact.parameter_list.clone())
            .collect();
    }
    written.dedup();
    if written.len() != 1 {
        return None;
    }
    let (parameters, _variadic) = crate::sema::overload::parameters_in(&written[0]);
    Some(parameters)
}
