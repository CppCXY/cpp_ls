//! **`argument-count`**: a call that passes more arguments than the callee takes, or fewer than it needs.
//!
//! ```cpp
//! int scale(int count, double factor);
//!
//! scale(1, 2.0);        // fine
//! scale(1);             // one argument for two parameters
//! scale(1, 2.0, 3);     // three for two
//! ```
//!
//! # Only a callee this file declares, and that is a fact about the analysis rather than a choice
//!
//! The parameter list of a function declared **here** is in this file's tree, and this check reads it there. For a
//! callee declared elsewhere the honest answer would need that file's tree, which a check is not given — see
//! [`CheckContext`]'s note on why it is handed a model and not a session — so a call into a header is **not
//! judged at all**, however wrong it looks.
//!
//! The field that looks like it would answer this — [`DeclFact::parameters`](crate::DeclFact::parameters) — must
//! not be used: it is the **template** parameter list (its own documentation says "a partial spelling of the
//! template parameters"), and the check this one replaces was measured reading it: on 200 MSVC headers it reported
//! **119 findings**, every one of them a function whose `parameters` was empty and whose real parameters were in
//! the tree all along.
//!
//! # `...`, defaults, and the two directions
//!
//! ```text
//!   too many     only when the callee is not variadic
//!   too few      only below the number of parameters that have no default
//! ```
//!
//! A default is recognised by the `=` the declaration writes, which is the only evidence in the tree — this crate
//! does not evaluate default arguments, so a parameter with one counts as satisfied whether or not the argument
//! could be formed. Erring that way is the direction to err in: a default that cannot be used is the compiler's
//! complaint, and this layer's job is to be quiet when it is not sure.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`argument-count`** — see the module documentation.
pub struct ArgumentCount;

/// What a callee declared in this file says about how it may be called.
#[derive(Clone)]
struct Callee {
    /// The parameters that have no default — a call below this many is missing arguments.
    required: usize,
    /// How many parameters are written, defaults included — a call above this many passes too many.
    written: usize,
    /// The declaration ends in `...`, so any number of arguments above `required` is allowed.
    variadic: bool,
    /// **The parameter types**, in order, with the parameter names dropped — what the overload resolver ranks a call
    /// against. `f(int)` and `f(const char*)` are `["int"]` and `["const char*"]`; `f(int n)` is `["int"]`.
    parameters: Vec<String>,
}

impl Check for ArgumentCount {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ArgumentCount];

    fn check(context: &mut CheckContext<'_>) {
        let callees = callees_in_this_file(context);

        // **The typed layer's own call node**, with its own accessors: `get_callee` and `get_args`. Both were being
        // guessed at before, and both guesses were wrong for this grammar — measured with `cpp_dump`, `scale(1, 2, 3)`
        // is
        //
        // ```text
        //   CallExpr   IdentifierExpr "scale"   "("   LiteralExpr 1   ","   LiteralExpr 2   ","   LiteralExpr 3   ")"
        // ```
        //
        // — there is **no `ArgumentList` node** for a simple call, so `children().find(kind == ArgumentList)` never
        // matched and this check reported nothing at all, in any file, for as long as it has existed.
        for call in context.own::<cpp_parser::CppCallExpr>().collect::<Vec<_>>() {
            let Some(callee) = call.get_callee() else {
                continue;
            };
            // **A member call is not a free function of that name.** Resolving the member needs the
            // object's type, which is a different question with its own false positives; until that
            // answer is trusted here, staying silent is the direction this module documents.
            if let Some(access) = super::member_access_above(callee.syntax()) {
                judge_a_member_call(context, &call, &access);
                continue;
            }

            // The **last name written** is what a declaration can be found by: `scale` in `scale(1)`, `scale` in
            // `w.scale(1)`, and the last of them in `ns::scale(1)`.
            let Some(name) = super::spelling_of(callee.syntax()) else {
                continue;
            };
            // **A name that is also a type is a constructor call.** Measured on EmmyLuaCodeStyle: `NameStyleRule(
            // NameStyleType::SnakeCase)` — **one** argument — was reported as "needs 2 argument(s) and this call passes
            // fewer", sixteen times, because the name is a class and a *function* fact of that name with two parameters
            // is in the index. Which constructor a call means is overload resolution among constructors, which this
            // check does not do; a name that names a type is silence rather than a wrong arity.
            //
            // Asked before **both** ways of finding a callee, because a function defined in this file takes the first
            // path and the index's facts are never consulted for it — the first version of this guard sat in the index
            // path and the fixture that reproduces the defect passed straight through it.
            if names_a_type(context, &name) {
                continue;
            }

            // **This file first, the index second.** A callee declared here is read from the tree, which is where its
            // defaults and its `...` are; one declared in another file is a **fact** the cook already produced —
            // `DeclFact::parameter_list`, the field whose own documentation says it is stored *"because a query holds
            // one file and the parameters are written in another"*.
            // **Every declaration of that name** — this file first, the index second. A name with several used to
            // be skipped whole; the set is what the resolver is for.
            let declared = match callees.get(name.as_str()) {
                Some(declared) if !declared.is_empty() => declared.clone(),
                _ => callees_from_the_index(context, &name),
            };
            if declared.is_empty() {
                continue;
            }

            // The arguments as the typed layer counts them — for `scale(1, 2, 3)`, three.
            let passed = call.get_args().len();

            // **Which declaration this call means, if one of them is the language's answer.** `One` is a candidate
            // that fits, so the arity below cannot fire against it. `None` is the only arity report that is a fact
            // about the program — nothing in view takes this shape — and it is reported against the **nearest**
            // declaration, so a message can still say "too many" or "too few" about something real. `Ambiguous` is
            // the language's own answer, and it is silence.
            let declared = match declared.as_slice() {
                [only] => only,
                several => {
                    // The engine answers with a parsed `Type` and the file it came from; the resolver ranks spellings,
                    // which is the same information in the form both the file half and the index half of this check
                    // have. An `Unknown` travels with its reason rather than becoming "no type".
                    let types: Vec<crate::Known<String>> = call
                        .get_args()
                        .iter()
                        .map(|argument| match context.type_of(argument.syntax()) {
                            crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                            crate::Known::No => crate::Known::No,
                            crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
                        })
                        .collect();
                    let candidates: Vec<crate::sema::overload::Candidate> = several
                        .iter()
                        .map(|callee| {
                            crate::sema::overload::Candidate::new(
                                callee.parameters.clone(),
                                callee.variadic,
                                callee.parameters.join(", "),
                            )
                        })
                        .collect();

                    match crate::sema::overload::choose(&types, &candidates) {
                        crate::sema::overload::Choice::One(chosen) => &several[chosen],
                        crate::sema::overload::Choice::None => match closest(several, passed) {
                            Some(closest) => closest,
                            None => continue,
                        },
                        crate::sema::overload::Choice::Ambiguous => continue,
                    }
                }
            };

            if !declared.variadic && passed > declared.written {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.get_range()),
                    name.clone(),
                    at_most(&name, declared.written),
                );
            } else if passed < declared.required {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.get_range()),
                    name.clone(),
                    at_least(&name, declared.required),
                );
            }
        }
    }
}

/// **Every function this file declares, by the name a call would write.** See [`Callee`].
///
/// Built once per check run: a file with a hundred calls and one declaration should read the declaration once, and
/// the map is what makes the cost the calls rather than the calls times the declarations.
fn callees_in_this_file(
    context: &CheckContext<'_>,
) -> std::collections::HashMap<String, Vec<Callee>> {
    let mut callees: std::collections::HashMap<String, Vec<Callee>> =
        std::collections::HashMap::new();

    // **The shared predicate**, and the parameters through the typed layer: measured, a file-scope definition is a
    // `Declaration` whose parameter list is a **grandchild** (`InitDeclarator > Declarator > ParameterList`), so a
    // filter on `FunctionDef` found nothing and a `children()` lookup for the parameter list found nothing either.
    // This check had therefore never judged a function of the file it was checking — the same defect shape as the
    // three before it, and the reason `super::function_definitions` exists.
    for node in super::function_definitions(context) {
        let Some(parameters) = cpp_parser::CppDeclaration::cast(node.clone())
            .and_then(|declaration| declaration.descendants::<cpp_parser::CppParamList>().next())
            .map(|parameters| parameters.syntax().clone())
        else {
            continue;
        };
        let Some(name) = super::name_declared_by(&node) else {
            continue;
        };

        let mut written = 0usize;
        let mut required = 0usize;
        let mut variadic = false;
        let mut spellings: Vec<String> = Vec::new();
        for parameter in parameters.children() {
            // **Read from the tokens**, not from the parameter's text: a `...` is a token, an `=` is a different
            // token, and a default value that happens to hold an `=` — `int n = a == b` — is a string of tokens
            // rather than a substring to search. Counting text is how this layer produced its one false positive,
            // and the fix everywhere has been the same.
            let kinds: Vec<cpp_parser::CppTokenKind> = parameter
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .map(|token| token.kind().into())
                .collect();

            if kinds.contains(&cpp_parser::CppTokenKind::Ellipsis) {
                variadic = true;
                continue;
            }
            written += 1;
            // **A default is the `=` the declaration writes** — see the module documentation for why nothing more
            // is asked of it.
            if !kinds.contains(&cpp_parser::CppTokenKind::Assign) {
                required += 1;
            }
            // **The parameter's own text.** `spelling_of` is for the declarator and expression nodes a declaration's
            // name is read from, and answers `None` for a parameter node — measured: two overloads of `f` with
            // different arities produced two candidates whose parameter lists were both **empty**, so the dedupe below
            // kept one of them and every call was judged against it. `text()` is the node's own source text.
            let written_as = parameter.text().to_string();
            spellings.push(crate::sema::overload::a_type_without_its_name(&written_as));
        }

        // **`(void)` is C's spelling of *no parameters*, and it is not one parameter of type `void`.** Counting it as
        // one made every `int f(void);` in a C project a function that needs an argument: measured on a nine-file C
        // corpus, `f(void)` called as `f()` produced **two** findings on one line — *"takes 1 argument(s) and this
        // call passes fewer"* from here, and *"is not declared"* from another reader. The rule is the language's: a
        // parameter list of exactly one unnamed `void` declares nothing.
        if written == 1 && spellings.len() == 1 && spellings[0].trim() == "void" {
            written = 0;
            required = 0;
            spellings.clear();
        }

        // A declaration and its definition name the same function; the same list twice is one candidate, not an
        // ambiguous call to something nobody overloaded.
        let declared = callees.entry(name).or_default();
        if !declared.iter().any(|known| known.parameters == spellings) {
            declared.push(Callee {
                required,
                written,
                variadic,
                parameters: spellings,
            });
        }
    }

    callees
}

fn at_most(name: &str, written: usize) -> String {
    t!(
        "`%{name}` takes %{written} argument(s) and this call passes more",
        name = name,
        written = written
    )
    .to_string()
}

/// **A callee the index knows** — its declaration is in another file, and its parameter list is a **fact**.
///
/// `DeclFact::parameter_list` is `(_Ty* _First, size_type _Count)`: parentheses, defaults, `...` and all, and it is
/// stored rather than read when wanted *"because a query holds one file and the parameters are written in
/// another"* — the sentence this function is an implementation of.
///
/// **One declaration or nothing.** Two declarations of one name are an overload set, and which of them a call
/// resolves to is overload resolution: picking one would report an arity against the wrong signature, so this
/// answers `None` for the whole set — the same rule [`super::a_class_can_be_built_from`] follows for constructors.
///
/// `None` as well for a declaration that is not a function, for one whose parameter list the reading could not
/// reach, and for a name the index does not hold at all.
fn callees_from_the_index(context: &CheckContext<'_>, name: &str) -> Vec<Callee> {
    let crate::Known::Yes(definitions) = context.index().definitions(name, context.path) else {
        return Vec::new();
    };
    // A class's constructor and a free function may share a spelling; only the functions are this rule's question.

    let functions: Vec<&crate::DeclFact> = definitions
        .found
        .iter()
        .map(|definition| &definition.fact)
        .filter(|fact| fact.kind == crate::DeclKind::Function)
        .collect();
    let mut callees: Vec<Callee> = Vec::new();
    for only in functions {
        let Some(parameters) = only.parameter_list.as_deref() else {
            continue;
        };
        let Some(callee) = the_callee_in(parameters) else {
            continue;
        };
        if !callees
            .iter()
            .any(|known| known.parameters == callee.parameters)
        {
            callees.push(callee);
        }
    }
    callees
}

/// **One candidate out of a `parameter_list` spelling** — the text side of [`callees_from_the_index`].
fn the_callee_in(parameters: &str) -> Option<Callee> {
    // **The parameters, split where the language splits them**: a comma at depth zero. `std::pair<int, int>` is one
    // parameter, and splitting on every comma would count it as two.
    let inner = parameters
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');
    let mut depth = 0i32;
    let mut written = 0usize;
    let mut required = 0usize;
    let mut variadic = false;
    let mut current = String::new();
    let mut spellings: Vec<String> = Vec::new();
    let take = |parameter: &str,
                written: &mut usize,
                required: &mut usize,
                variadic: &mut bool,
                spellings: &mut Vec<String>| {
        let parameter = parameter.trim();
        if parameter.is_empty() {
            return;
        }
        // **A pack is a `...` that is not the whole parameter, and this test used to miss every real spelling of it.**
        //
        // The file-scope half above asks the **tokens** of each parameter for an `Ellipsis`, so `Args&&... args` marks
        // the callee variadic there. Here the parameter list is text — it is a `DeclFact` produced by the cook, read
        // when the declaration lives in another file — and the test was `parameter.ends_with("...")`, which is true of
        // the C-style `...` and of `Args...` and of **nothing else people write**. Measured shape of the defect:
        // `template <class... Args> void f(Args&&... args)` recorded one written, one required parameter, so a call
        // `f(1, 2, 3)` was reported as *"takes 1 argument(s) and this call passes more"*.
        //
        // The rule is now the file half's rule — an ellipsis anywhere in the parameter means the parameter (or the
        // callee) is a pack — with one guard the token half gets for free: an ellipsis inside a literal is text, not a
        // pack, so `const char* dots = "..."` does not make a callee variadic.
        if has_an_ellipsis(parameter) {
            *variadic = true;
            return;
        }
        *written += 1;
        // A default is the `=` the declaration writes.
        if !parameter.contains('=') {
            *required += 1;
        }
        // The type, with any default and any parameter name dropped — see `a_type_without_its_name`.
        let without_default = parameter.split('=').next().unwrap_or(parameter).trim();
        spellings.push(crate::sema::overload::a_type_without_its_name(
            without_default,
        ));
    };
    for character in inner.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                take(
                    &current,
                    &mut written,
                    &mut required,
                    &mut variadic,
                    &mut spellings,
                );
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    take(
        &current,
        &mut written,
        &mut required,
        &mut variadic,
        &mut spellings,
    );

    // **`(void)` is the C spelling of *no parameters*** — the same rule the token half applies, and it has to be
    // applied here too or a declaration read from **another file** keeps the wrong arity: `int f(void);` answered one
    // parameter, so a call `f()` in a file that merely includes the header was reported as passing too few.
    if written == 1 && spellings.len() == 1 && spellings[0].trim() == "void" {
        written = 0;
        required = 0;
        spellings.clear();
    }

    Some(Callee {
        required,
        written,
        variadic,
        parameters: spellings,
    })
}

/// **The candidate to blame when none is viable**: the one whose arity is nearest the call's, so a message about a
/// call that fits nothing can still say "too many" or "too few" about a real declaration.
fn closest(candidates: &[Callee], passed: usize) -> Option<&Callee> {
    candidates.iter().min_by_key(|callee| {
        if passed > callee.written {
            passed - callee.written
        } else {
            callee.required.saturating_sub(passed)
        }
    })
}

fn at_least(name: &str, required: usize) -> String {
    t!(
        "`%{name}` needs %{required} argument(s) and this call passes fewer",
        name = name,
        required = required
    )
    .to_string()
}
/// **Is there a `...` in this parameter that the language would call one?**
///
/// The parameter list of a declaration in another file arrives as text, so this is a scan rather than a token walk —
/// and the one thing a scan gets wrong is an ellipsis written inside a literal. `void log(const char* dots = "...")`
/// is not variadic, and `standardese`'s own `Args... args` is. Quotes are tracked with a backslash escape and no
/// attempt at raw strings: a raw string holding an odd number of quotes is the one case this misreads, and it errs
/// towards calling the callee variadic, which is the quiet direction.
fn has_an_ellipsis(parameter: &str) -> bool {
    let bytes = parameter.as_bytes();
    let mut at = 0usize;
    let mut quoted: Option<u8> = None;

    while at < bytes.len() {
        let byte = bytes[at];
        match quoted {
            Some(quote) => {
                if byte == b'\\' {
                    at += 2;
                    continue;
                }
                if byte == quote {
                    quoted = None;
                }
            }
            None => {
                if byte == b'"' || byte == b'\'' {
                    quoted = Some(byte);
                } else if parameter[at..].starts_with("...") {
                    return true;
                }
            }
        }

        at += 1;
    }

    false
}

/// **Is this name a type as well as a function?** — see the guard that calls this.
fn names_a_type(context: &CheckContext<'_>, name: &str) -> bool {
    let crate::Known::Yes(definitions) = context.index().definitions(name, context.path) else {
        return false;
    };
    definitions
        .found
        .iter()
        .any(|definition| definition.fact.kind != crate::DeclKind::Function)
}

/// **A member call, judged against the member it names.**
///
/// The guard above used to skip every member call, because `_session.Run(…)` was being judged against a free `Run()` and
/// reported 19 times on a real project. Skipping was the right *direction* and the wrong *answer*: the member is
/// resolvable — `member_access_at` gives the object, the object's type gives the member list, and the member gives the
/// parameter list — so the call can be judged instead of ignored.
///
/// Any overload that accepts this many arguments answers it; only when none does is the closest reported, which is the
/// same rule the free-function path uses.
fn judge_a_member_call(
    context: &mut CheckContext<'_>,
    call: &cpp_parser::CppCallExpr,
    access: &crate::sema::resolve::MemberAccess,
) {
    let crate::Known::Yes((a_type, _)) = context.type_of(&access.object) else {
        return;
    };
    let crate::Known::Yes(list) = context.model.members_of(&a_type.to_string()) else {
        return;
    };
    let members: Vec<Callee> = list
        .members
        .iter()
        .filter(|member| member.fact.name == access.member)
        .filter_map(|member| member.fact.parameter_list.as_deref())
        .filter_map(the_callee_in)
        .collect();
    if members.is_empty() {
        return;
    }

    let passed = call.get_args().len();
    if members
        .iter()
        .any(|member| member.variadic || (passed >= member.required && passed <= member.written))
    {
        return;
    }
    let Some(declared) = closest(&members, passed) else {
        return;
    };

    let name = access.member.clone();
    context.add(
        DiagnosticCode::ArgumentCount,
        cpp_parser::source_range(call.get_range()),
        name.clone(),
        if passed > declared.written {
            at_most(&name, declared.written)
        } else {
            at_least(&name, declared.required)
        },
    );
}
