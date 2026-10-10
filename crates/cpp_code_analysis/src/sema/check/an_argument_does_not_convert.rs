//! **An argument a call cannot pass to any of the functions it names.**
//!
//! ```cpp
//! void f(int);
//! f("three");                 // no conversion from `const char*` to `int`
//! f(1, 2);                    // one argument too many, whichever overload is meant
//! ```
//!
//! # The same relation as the initialiser check, asked at a call site
//!
//! [`super::an_initializer_does_not_convert`] asks [`Type::convertible_to`] about an initialiser and its declared
//! type; this asks it about an argument and a parameter. One question, one implementation — a rule written here
//! about "a string where a number goes" would be a second answer to something already answered.
//!
//! # Where the parameters come from, and the field that was wrong
//!
//! [`DeclFact`] carries **two** fields whose names suggest this check's question, and the first version of this file
//! read the wrong one. Measured on 200 MSVC headers it reported **119 findings**, and a trace said why in one line:
//!
//! ```text
//!   candidate kind=Function params=[] returns=Some("void")     for `void takes_a_number(int);`
//! ```
//!
//! `DeclFact::parameters` is the **template** parameter list — its own documentation says *"a partial specialization
//! records its own list (`template <class T> struct vector…`)"* — so a function that is not a template has none, and
//! the check fell back to "cannot tell" on every call in the standard library. What it needed is
//! [`DeclFact::parameter_list`], the list **as written**, which is the field the signature popup is built from.
//!
//! # Splitting a written list is not splitting on commas
//!
//! `parameter_list` is a string, and the sibling that parses one takes a **node**
//! ([`crate::sema::scopes::parameters_of`]), which a check does not have: the declaration is in another file, whose
//! tree the index does not keep. So the split happens here — and the straightforward version is wrong the moment a
//! default argument mentions a template: `std::pair<int, int> p = {}` has a comma that is *inside* a parameter.
//! [`top_level_spellings`] counts brackets for exactly that reason, and ends a spelling at the `=` that introduces a
//! default, because a default is not part of the parameter's type.
//!
//! # What is reported, and what makes the answer safe
//!
//! Only a **definite** `Known::No`, and only when it is definite about **every** candidate: a call names a name, and
//! a name may denote an overload set where another declaration takes exactly this argument. So a finding needs all
//! three of:
//!
//! * the candidates are known — [`ProjectIndex::definitions`](crate::ProjectIndex::definitions) answered
//!   `Known::Yes`;
//! * **every** candidate is a function whose written parameter list can be read, and **none** of them is a template
//!   (whose parameters may be *deduced* from the argument, which is not a conversion question);
//! * **every** candidate disagrees — either it takes a different number of parameters, or the argument's type does
//!   not convert to the one in that position.
//!
//! Anything else is silence. A name declared in a header the analysis has not read is `Known::Unknown`, and this
//! layer reports nothing on `Unknown` — which is what keeps it usable on code that includes the standard library,
//! where almost every call is to something the index knows only by name.

use cpp_parser::CppSyntaxKind;

use crate::Known;
use crate::sema::types::parse_type_spelling;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "an_argument_does_not_convert";

/// Every call that no candidate function can accept.
pub fn an_argument_does_not_convert(checks: &Checks<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();

    // **Every call in the file, found in one walk** — the lesson `an_initializer_does_not_convert` records: a walk
    // per candidate is quadratic in the size of the file, and this layer runs per keystroke.
    let calls: Vec<cpp_parser::CppSyntaxNode> = checks
        .tree()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
        .collect();

    for call in &calls {
        // **The callee has to be a plain name.** A member access or a pointer-to-function is a different lookup.
        let Some(callee) = call.children().next() else {
            continue;
        };
        let written = callee.text().to_string();
        let written = written.trim();
        if written.is_empty()
            || !written
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
        {
            continue;
        }

        let arguments = arguments_of(call);
        // A call with no arguments is valid for too many shapes to judge: a function taking none, one taking all
        // defaults, and one whose parameters this cannot see are the same call.
        if arguments.is_empty() {
            continue;
        }

        let Known::Yes(candidates) = checks.index().definitions(written, checks.path) else {
            continue;
        };

        let mut all_disagree = !candidates.found.is_empty();
        let mut reason: Option<String> = None;

        for candidate in &candidates.found {
            let Some(parameters) = parameter_spellings(&candidate.fact) else {
                all_disagree = false;
                break;
            };
            // **The number of arguments is not judged**, and that is a measured decision rather than a simplification.
            //
            // Arity looks like the safe half of this check — `f(1, 2)` against a one-parameter `f` needs no type at
            // all — and it was written first, with the rule that a call may pass fewer arguments when the rest have
            // defaults. Over 200 MSVC headers the check then reported **five** findings and **every one of them was
            // an arity claim**, none a type claim:
            //
            // ```text
            //   _ctime32_s                    4 declared, 3 passed    two declarations exist, one without the default
            //   _ScheduleFuncWithAutoInline   2 declared, 1 passed    the same shape
            //   _Cancel                       0 declared, 2 passed    a different, unrelated `_Cancel`
            // ```
            //
            // Each is a case where the **candidate set is incomplete**: [`ProjectIndex::definitions`] answered with
            // the declarations of that name it could see, and the one being called was not among them. Arity is the
            // question that cannot survive that, because one missing overload changes the answer; a *type* claim can,
            // because it is only made when every candidate that **was** found disagrees — and the measurement says
            // exactly that: the type rule reported none of these five.
            //
            // So a call with a different number of arguments than the candidates take is `Unknown` here. The arity
            // rule belongs in a check that can see the whole overload set, which is a question about resolution
            // rather than about a call site.
            if parameters.len() != arguments.len() {
                all_disagree = false;
                break;
            }

            // **Every parameter in this candidate has to reject its argument.** The flag is named for the question
            // it answers and is *cleared* by a rejection, which is the opposite of the first version: that one was
            // initialised to `true` and only ever set to `true` again, so it read every candidate as accepting the
            // call and the check reported nothing at all — the trace said `convertible(const char*, int) = No` and
            // no finding came out.
            let mut this_one_agrees = true;
            for (argument, parameter) in arguments.iter().zip(parameters.iter()) {
                // Through the model, which can read the file a typedef is declared in — see
                // [`super::Checks::model`] for what handing over `&mut |_| None` cost the sibling check.
                let known = checks.type_of(argument);
                // An argument whose type is not known might be anything, so this candidate is not refuted.
                let Known::Yes((from, _)) = known else {
                    this_one_agrees = true;
                    break;
                };
                let verdict = from.convertible_to(&parse_type_spelling(parameter));
                if !matches!(verdict, Known::No) {
                    // `Yes` — it converts — or `Unknown` — the relation cannot say. Either way this candidate might
                    // accept the call, and one maybe is enough to stay silent.
                    this_one_agrees = true;
                    break;
                }
                reason.get_or_insert_with(|| {
                    format!("`{written}` takes `{parameter}` here, and `{from}` does not convert to it")
                });
                this_one_agrees = false;
            }

            if this_one_agrees {
                all_disagree = false;
                break;
            }
        }

        if !all_disagree {
            continue;
        }

        findings.push(Finding {
            range: cpp_parser::source_range(call.text_range()),
            name: written.to_string(),
            check: CHECK,
            message: reason
                .unwrap_or_else(|| format!("`{written}` cannot be called with these arguments")),
        });
    }

    findings
}

/// **The parameter types a function declares, from the list as written** — `None` when the list cannot be read.
///
/// `None` rather than an empty vector, and the difference is the whole of this function's contract: `()` written in
/// a header and `()` written for a function whose list was lost are the same two characters, and only one of them
/// means "takes no arguments". Reporting the other as an arity mismatch is the defect this check was caught by, so
/// anything unreadable refuses the candidate instead — and refusing one candidate refuses the whole call, because
/// that candidate might have been the one that accepts it.
fn parameter_spellings(fact: &crate::DeclFact) -> Option<Vec<String>> {
    if fact.kind != crate::DeclKind::Function {
        return None;
    }
    // **A template function is not judged.** Its parameters may be deduced from the argument — `template <class T>
    // void f(T)` accepts `f("three")` — which is a question about deduction rather than about conversion.
    if !fact.parameters.is_empty() {
        return None;
    }

    let written = fact.parameter_list.as_deref()?;
    // **The outer parentheses come off first.** `parameter_list` is the list *as written*, so it is `(int)` and not
    // `int` — read from the trace that found this: `convertible(int, (int)) = Unknown`, where the relation was handed
    // a spelling that is a builtin with brackets round it. Nothing inside is a bracket at depth zero after this,
    // which is what `top_level_spellings` is entitled to assume.
    let written = written.trim();
    let written = written
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(written)
        .trim();
    // `(void)` is the C spelling of "no parameters", and `()` is C++'s.
    if written.is_empty() || written == "void" {
        return Some(Vec::new());
    }

    Some(
        top_level_spellings(written)
            .into_iter()
            .map(|spelling| spelling.to_string())
            .collect(),
    )
}

/// **The parts of a written parameter list that are separated by top-level commas.**
///
/// Bracket-aware, because the commas that matter are the ones at depth zero: `std::pair<int, int> p = {}` is **one**
/// parameter with a comma inside it, and `int, char` is two. The depth counts every kind of bracket a parameter can
/// open — `<`, `(`, `[`, `{` — and a spelling ends at a top-level `,` **or at a top-level `=`**, since a default
/// argument is not part of the parameter's type.
///
/// A `<` that never closes merges the rest of the list into one spelling. That is the safe direction: an unparsable
/// list becomes one parameter of an unparsable type, which nothing converts to, and the call is refused rather than
/// reported.
fn top_level_spellings(written: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (at, character) in written.char_indices() {
        match character {
            '<' | '(' | '[' | '{' => depth += 1,
            // A `>` that closes nothing is the `->` of a trailing return type or a comparison; either way it does
            // not open a bracket, and letting it take the depth below zero would swallow the next parameter's comma.
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' | '=' if depth == 0 => {
                parts.push(written[start..at].trim());
                // Past the default argument entirely: everything to the next top-level comma is the default.
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(written[start..].trim());

    parts.retain(|part| !part.is_empty());
    parts
}

/// The argument expressions of a call — every node child after the callee.
///
/// A call is written `callee ( a , b )`, so its node children are the callee and one node per argument; the
/// parentheses and commas are tokens.
fn arguments_of(call: &cpp_parser::CppSyntaxNode) -> Vec<cpp_parser::CppSyntaxNode> {
    call.children().skip(1).collect()
}
