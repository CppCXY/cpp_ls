#![allow(dead_code)]
//! **Function template argument deduction** — what `T` is, given the call.
//!
//! ```cpp
//! template <typename T> void f(T value);
//! f(1);                     // T is int, and every question about this call is a question about `f(int)`
//! ```
//!
//! A template is not a function until it is instantiated, and the call is what instantiates it. The resolver in
//! [`crate::sema::overload`] ranks **spellings**, so a template candidate has to arrive at it already substituted —
//! that is what this module produces: the template arguments, in the order the template declares them, which
//! [`crate::sema::types::substituted_spelling`] turns into the parameter types the call is really facing.
//!
//! # The subset, and why it is small on purpose
//!
//! ```text
//! deduced     `T`                 the parameter is exactly the parameter
//!             `const T&` / `T&&`  the same type seen through a reference and a cv-qualifier
//!             `T*` / `T&` / `T[]` a shape the argument has and the parameter names
//! deduced     `T` twice           if both agree, once; if they disagree, not at all — the call does not compile
//! refused     anything else       `std::vector<T>`, `T::value_type`, a pack. Each is a real rule of the language and
//!                                 each needs a unifier that this module does not have; refusing gives the caller a
//!                                 candidate it can rank as `Unranked`, which is the quiet direction, instead of a
//!                                 substitution that is wrong for some calls and right for others
//! ```
//!
//! # Why a failure is `None` and not a partial answer
//!
//! A substitution is used to *rank* a call and, in signature help, to *show* a reader what the callee's parameters are.
//! A half-deduced template would show `f(int, T)` for a call whose second parameter is `double`, which is worse than
//! showing `f(T, T)` — the reader of the second can see that the analysis did not deduce, and the reader of the first
//! cannot.

use std::collections::HashMap;

use crate::sema::symbol::Known;

/// **The template arguments a call fixes**, in the order the template declares them.
///
/// `None` when any declared parameter stays unbound, when two deductions of one parameter disagree, or when the call
/// gives too few arguments to reach every parameter that mentions one.
pub(crate) fn deduce(
    names: &[String],
    parameters: &[String],
    arguments: &[Known<String>],
) -> Option<Vec<String>> {
    if names.is_empty() {
        return None;
    }

    let mut bound: HashMap<String, String> = HashMap::new();

    for (parameter, argument) in parameters.iter().zip(arguments) {
        let Known::Yes(argument) = argument else {
            // Nothing is known about this argument, so this parameter cannot bind anything. A later parameter may still
            // bind it — `f(T, const T&)` called with an unknown and an `int` is a deduction this can make.
            continue;
        };

        let parameter = the_shape_of(parameter);
        let argument = the_shape_of(argument);

        // **The parameter is exactly a template parameter.** The whole of the commonest case.
        if names.iter().any(|name| name == &parameter) {
            if !bind(&mut bound, &parameter, &argument) {
                return None;
            }
            continue;
        }

        // **The parameter names a template parameter inside a shape**: `T*`, `T&`, `T[]`. The argument has to have the
        // same shape before and after the name, and the part where the name was is its argument.
        if let Some((before, name, after)) = around_a_name(&parameter, names) {
            let Some(rest) = argument.strip_prefix(&before) else {
                continue;
            };
            let Some(rest) = rest.strip_suffix(&after) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            if !bind(&mut bound, &name, rest) {
                return None;
            }
            continue;
        }

        // Anything else mentions no Template parameter in a way this can split, and binds nothing.
    }

    // Every declared parameter must have a value: a substitution missing one is not an instantiation.
    let mut deduced = Vec::with_capacity(names.len());
    for name in names {
        deduced.push(bound.get(name.as_str())?.clone());
    }
    Some(deduced)
}

/// Bind one name, and refuse a second deduction that disagrees — `f(T, T)` called with `int` and `long` does not
/// compile, and the analysis should not pretend it does.
fn bind(bound: &mut HashMap<String, String>, name: &str, argument: &str) -> bool {
    match bound.get(name) {
        Some(known) => known == argument,
        None => {
            bound.insert(name.to_string(), argument.to_string());
            true
        }
    }
}

/// **`T*` as `("", "T", "*")`** — the text before and after the first template parameter the spelling names.
fn around_a_name(parameter: &str, names: &[String]) -> Option<(String, String, String)> {
    for name in names {
        let mut from = 0usize;
        while let Some(at) = parameter[from..].find(name.as_str()) {
            let start = from + at;
            let end = start + name.len();
            let before = &parameter[..start];
            let after = &parameter[end..];
            // Only a **whole word**: `T` must not match inside `Type`.
            let whole_start = start == 0
                || !parameter[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_alphanumeric() || character == '_');
            let whole_end = end == parameter.len()
                || !parameter[end..]
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_alphanumeric() || character == '_');
            if whole_start && whole_end && !(before.is_empty() && after.is_empty()) {
                return Some((
                    before.trim().to_string(),
                    name.clone(),
                    after.trim().to_string(),
                ));
            }
            from = end;
        }
    }
    None
}

/// A spelling without the reference and cv-qualifiers a call does not choose on — the same normalisation the resolver
/// ranks with, kept here so that a deduction and a rank agree about what two types are.
fn the_shape_of(spelling: &str) -> String {
    let mut text = spelling.trim().to_string();
    loop {
        let trimmed = text.trim();
        if let Some(before) = trimmed
            .strip_suffix("&&")
            .or_else(|| trimmed.strip_suffix('&'))
        {
            text = before.trim_end().to_string();
        } else {
            break;
        }
    }
    loop {
        let trimmed = text.trim_start();
        if let Some(after) = trimmed
            .strip_prefix("const ")
            .or_else(|| trimmed.strip_prefix("volatile "))
        {
            text = after.trim_start().to_string();
        } else {
            break;
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// **The template parameters a declaration owns**, read from the text before it.
///
/// A heuristic, and stated as one — the same one `signature.rs` grew for the same reason:
/// `DeclFact` has no template header, so the nearest `template` before the declaration's name is
/// the only evidence there is.
pub fn template_parameters_before(source: &str, name_offset: usize) -> Vec<String> {
    let before = &source[..name_offset.min(source.len())];
    let Some(at) = before.rfind("template") else {
        return Vec::new();
    };
    let header = &before[at..];
    let Some(open) = header.find('<') else {
        return Vec::new();
    };
    let Some(close) = header[open..].find('>') else {
        return Vec::new();
    };
    if header[open + close..].contains(';') {
        return Vec::new();
    }
    let inside = &header[open + 1..open + close];

    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut part = String::new();
    for character in inside.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                if let Some(name) = the_name_in(&part) {
                    names.push(name);
                }
                part.clear();
                continue;
            }
            _ => {}
        }
        part.push(character);
    }
    if let Some(name) = the_name_in(&part) {
        names.push(name);
    }
    names
}

/// The last identifier of one `template` parameter, which is its name: `typename T` → `T`, `int N` → `N`.
fn the_name_in(parameter: &str) -> Option<String> {
    let without_default = parameter.split('=').next().unwrap_or(parameter);
    let identifier = without_default
        .split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .rfind(|word| !word.is_empty())?;
    Some(identifier.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(spelling: &str) -> Known<String> {
        Known::Yes(spelling.to_string())
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn parameters(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn the_parameter_that_is_just_the_parameter_is_the_commonest_case() {
        assert_eq!(
            deduce(&names(&["T"]), &parameters(&["T"]), &[yes("int")]),
            Some(names(&["int"]))
        );
    }

    #[test]
    fn a_reference_and_a_cv_qualifier_are_the_same_type_seen_two_ways() {
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["const T&"]),
                &[yes("std::string")]
            ),
            Some(names(&["std::string"]))
        );
    }

    #[test]
    fn a_shape_is_carried_by_the_argument_and_the_name_is_what_is_inside_it() {
        assert_eq!(
            deduce(&names(&["T"]), &parameters(&["T*"]), &[yes("int*")]),
            Some(names(&["int"]))
        );
    }

    #[test]
    fn a_name_deduced_twice_has_to_agree() {
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["T", "T"]),
                &[yes("int"), yes("int")]
            ),
            Some(names(&["int"])),
            "two deductions that agree are one deduction"
        );
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["T", "T"]),
                &[yes("int"), yes("long")]
            ),
            None,
            "and two that disagree do not compile, so this analysis should not pretend they do"
        );
    }

    #[test]
    fn an_argument_nothing_is_known_about_binds_nothing_and_a_later_one_can_still_bind_it() {
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["T", "const T&"]),
                &[
                    Known::Unknown(crate::sema::symbol::UnknownReason::DependentName),
                    yes("int")
                ],
            ),
            Some(names(&["int"])),
            "the second parameter still fixes T"
        );
    }

    #[test]
    fn a_name_that_stays_unbound_is_a_refusal_and_not_a_guess() {
        assert_eq!(
            deduce(&names(&["T"]), &parameters(&["T*"]), &[yes("int")]),
            None,
            "`int` is not a `T*`"
        );
        assert_eq!(
            deduce(&names(&["T"]), &parameters(&["int"]), &[yes("int")]),
            None,
            "a parameter that names no template parameter deduces nothing"
        );
    }

    #[test]
    fn a_single_level_template_id_is_deduced_by_what_surrounds_the_name() {
        // The split `around_a_name` makes is "everything before the name" and "everything after it", so a one-level
        // template-id is a shape like any other: `vector<T>` against `vector<int>` leaves `int` where `T` was. The
        // first version of this test asserted a refusal here, which was wrong about what this can do.
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["std::vector<T>"]),
                &[yes("std::vector<int>")]
            ),
            Some(names(&["int"]))
        );
    }

    #[test]
    fn a_shape_the_argument_does_not_have_is_refused_rather_than_approximated() {
        assert_eq!(
            deduce(
                &names(&["T"]),
                &parameters(&["std::vector<T>"]),
                &[yes("std::map<int, int>")]
            ),
            None,
            "`std::map<int, int>` does not begin with `std::vector<`, so nothing binds and the answer is no"
        );
    }
}
