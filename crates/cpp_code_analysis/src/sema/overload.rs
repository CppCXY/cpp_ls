#![allow(dead_code)]
//! **Which overload a call means** — the resolution that `param-type-check`'s documentation says a check must not
//! imitate.
//!
//! ```text
//! void f(int);            // this file's own words, in `param_type_check.rs`:
//! void f(double);         //
//! f(1);                   //   "a name that denotes an **overload set** is a call whose meaning is the language's
//!                         //    resolution, and a check with a rule of its own would be wrong about every overload it
//!                         //    did not pick"
//! ```
//!
//! That was right, and it is why two checks currently say nothing at all about an overload set: there was no resolver
//! for them to agree with. This module is that resolver, for the subset of the language the checks act on.
//!
//! # The rule, and how much of C++ it is
//!
//! A call chooses among the candidates that are **viable** — every argument can go to its parameter and the arity
//! works — and among those, the one that is **at least as good in every argument and better in at least one**. If no
//! candidate is strictly better than all others, the call is ambiguous and the answer is [`Choice::Ambiguous`], which
//! a check must treat as "nothing to say" rather than as "no candidate".
//!
//! What is deliberately not here, because a checker does not need it and guessing it would produce findings:
//!
//! ```text
//! user-defined conversions        a class with a converting constructor is `Known::Unknown`, not a ranking
//! templates                       a template candidate is not ranked here; it is deduced elsewhere
//! reference binding rules         `const T&` and `T` are the same argument written two ways, not two ranks
//! ```
//!
//! # Why `Unranked` is a rank
//!
//! An argument whose type is `Known::Unknown` — a template, a type from a header nobody read — is not evidence
//! against a candidate. Ranking it as `NotViable` would silently delete the overload the program actually chose;
//! ranking it as `Exact` would let it win an argument it knows nothing about. So it is its own rank: it neither wins
//! nor loses, and a candidate that has one can win only by being the sole viable candidate.

use crate::sema::symbol::Known;
use crate::sema::types::parse_type_spelling;

/// **How well one argument matches one parameter**, best first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Match {
    /// The parameter is the argument's own type, however the two were written: `T`, `const T&` and `T&&` are one type
    /// seen through three spellings, and a call does not choose between them on that.
    Exact,
    /// A promotion — what widening costs nothing: `char`/`short` to `int`, `float` to `double`.
    Promotion,
    /// A conversion that is real but not free: `int` to `double`, `Derived*` to `Base*`, an array to a pointer.
    Conversion,
    /// Nothing is known about the argument, so nothing can be ranked — see the module documentation.
    Unranked,
    /// This argument cannot go to that parameter at all.
    NotViable,
}

/// **One candidate** — a declaration a call could be calling.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    /// The parameter spellings, in order. A spelling rather than a [`crate::sema::types::Type`] because the callers
    /// have spellings and the parse is cheap; see [`rank`].
    pub(crate) parameters: Vec<String>,
    /// Does the parameter list end in `...`? Then any number of arguments may follow.
    pub(crate) variadic: bool,
    /// What to show a reader when this candidate is chosen: the spelling of the signature.
    pub(crate) signature: String,
    /// **A function template.** A tie between a template and an ordinary function goes to the ordinary one — see
    /// [`choose`] — which is the language's rule and the reason `f(1)` means `f(int)` when both `f(int)` and a
    /// `template <typename T> f(T)` are in view.
    pub(crate) is_a_template: bool,
    /// **What the declaration wrote**, before substitution — the spellings partial ordering compares. Empty for an
    /// ordinary function.
    pub(crate) declared: Vec<String>,
    /// The template's parameter names, for deducing one declaration's spellings from another's.
    pub(crate) template_parameters: Vec<String>,
}

impl Candidate {
    /// The candidate an ordinary declaration writes: `void f(int, ...)` as `["int"]` and `variadic`.
    pub(crate) fn new(parameters: Vec<String>, variadic: bool, signature: String) -> Self {
        Self {
            parameters,
            variadic,
            signature,
            is_a_template: false,
            declared: Vec::new(),
            template_parameters: Vec::new(),
        }
    }

    /// **The same candidate, marked as a template** — the one difference a tie is broken on.
    pub(crate) fn as_a_template(mut self) -> Self {
        self.is_a_template = true;
        self
    }

    /// **The same candidate, with what the declaration wrote** — the spellings and the template names partial ordering
    /// needs, kept beside the substituted ones the ranking uses.
    pub(crate) fn with_declaration(mut self, declared: Vec<String>, template_parameters: Vec<String>) -> Self {
        self.is_a_template = !template_parameters.is_empty();
        self.declared = declared;
        self.template_parameters = template_parameters;
        self
    }

    /// **Can a call with this many arguments reach this candidate?**
    ///
    /// One rule is stated in the conservative direction: a parameter list that is longer than the argument list is
    /// **viable**, because the extra parameters may have defaults and the default arguments are not in the spelling
    /// this reads. Treating them as "no" would delete the overload a program chose — `void f(int, int = 0);` is a
    /// real candidate for `f(1)`.
    fn takes(&self, arguments: usize) -> bool {
        self.variadic || arguments <= self.parameters.len()
    }
}

/// **What the resolution concluded.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Choice {
    /// Exactly one candidate is the best: its index.
    One(usize),
    /// More than one candidate is equally good — the program is ambiguous, and a check has nothing to say.
    Ambiguous,
    /// No candidate is viable: the call does not fit any declaration that is in view.
    None,
}

/// **Resolve a call** against the candidates in view.
///
/// The arguments are the types the engine already worked out, in source order; `Known::Unknown` is an argument whose
/// type is not known and is handled by [`Match::Unranked`].
pub(crate) fn choose(argument_types: &[Known<String>], candidates: &[Candidate]) -> Choice {
    let viable: Vec<usize> = (0..candidates.len())
        .filter(|index| candidates[*index].takes(argument_types.len()))
        .filter(|index| {
            let candidate = &candidates[*index];
            argument_types
                .iter()
                .enumerate()
                .all(|(at, argument)| {
                    // A variadic candidate has no parameter to rank against beyond its own list.
                    match candidate.parameters.get(at) {
                        Some(parameter) => rank(parameter, argument) != Match::NotViable,
                        None => true,
                    }
                })
        })
        .collect();

    if viable.is_empty() {
        return Choice::None;
    }
    if viable.len() == 1 {
        return Choice::One(viable[0]);
    }

    // **The partial order, and nothing stronger.** A candidate wins if it is at least as good as every other in every
    // argument and strictly better in at least one. Two candidates that tie everywhere are ambiguous — that is the
    // language's answer too, and a check must not pick one of them.
    let ranks = |index: usize| -> Vec<Match> {
        let candidate = &candidates[index];
        argument_types
            .iter()
            .enumerate()
            .map(|(at, argument)| match candidate.parameters.get(at) {
                Some(parameter) => rank(parameter, argument),
                None => Match::Exact,
            })
            .collect()
    };
    let ranked: Vec<(usize, Vec<Match>)> = viable.iter().map(|index| (*index, ranks(*index))).collect();

    // **The candidates nothing beats.** A candidate is out when another is strictly better than it; everything left is
    // a tie, and a tie is what the language calls ambiguous — with one exception, below.
    let strictly_better = |mine: &[Match], theirs: &[Match]| -> bool {
        better_or_equal(mine, theirs) && !better_or_equal(theirs, mine)
    };
    let maximal: Vec<usize> = ranked
        .iter()
        .filter(|(index, mine)| {
            !ranked
                .iter()
                .any(|(other, theirs)| other != index && strictly_better(theirs, mine))
        })
        .map(|(index, _)| *index)
        .collect();

    if maximal.len() == 1 {
        return Choice::One(maximal[0]);
    }

    // **A non-template is the better function when a template is equally good** — the language's rule, and the one that
    // makes `f(1)` mean `f(int)` when `f(T)` is in view too. Without it that call is "ambiguous", which is safe and is
    // not what the program says.
    let ordinary: Vec<usize> = maximal
        .iter()
        .copied()
        .filter(|index| !candidates[*index].is_a_template)
        .collect();
    let maximal = if ordinary.is_empty() { maximal } else { ordinary };

    // **The more specialized template wins.** Two templates can deduce to the same parameter types — `take(T*)` and
    // `take(T)` both become `int*` for an `int*` — and the language settles that by **partial ordering**: the more
    // specialized one. A template is at least as specialized as another when the other's spellings can be deduced
    // against its own, which is a comparison of the two declarations and never of the call.
    if maximal.len() > 1 {
        let ordinary: Vec<usize> = maximal
            .iter()
            .copied()
            .filter(|mine| {
                maximal
                    .iter()
                    .all(|other| other == mine || !more_specialized(&candidates[*other], &candidates[*mine]))
            })
            .collect();
        if !ordinary.is_empty() && ordinary.len() < maximal.len() {
            return match ordinary.as_slice() {
                [only] => Choice::One(*only),
                _ => Choice::Ambiguous,
            };
        }
    }

    // **Fewer parameters used is better** — `f(int)` beats `f(int, int = 0)` for `f(1)`, because the second needs a
    // default argument and a call pays for a default it does not have to. This is a real rule of the language, not a
    // tie-break invented here; the alternative is calling every call to a defaulted overload ambiguous.
    let fewest = maximal
        .iter()
        .map(|index| candidates[*index].parameters.len())
        .min()
        .unwrap_or(0);
    let leaders: Vec<usize> = maximal
        .iter()
        .copied()
        .filter(|index| candidates[*index].parameters.len() == fewest)
        .collect();

    match leaders.as_slice() {
        [only] => Choice::One(*only),
        _ => Choice::Ambiguous,
    }
}
/// `mine` is at least as good as `theirs` in every argument.
fn better_or_equal(mine: &[Match], theirs: &[Match]) -> bool {
    mine.iter().zip(theirs).all(|(mine, theirs)| mine <= theirs)
}

/// `mine` is strictly better in at least one argument that either of them can rank.
fn better(mine: &[Match], theirs: &[Match]) -> bool {
    mine.iter()
        .zip(theirs)
        .any(|(mine, theirs)| mine < theirs && *mine != Match::Unranked && *theirs != Match::Unranked)
}

/// **How well one argument matches one parameter** — see [`Match`].
pub(crate) fn rank(parameter: &str, argument: &Known<String>) -> Match {
    let argument = match argument {
        Known::Yes(argument) => argument,
        Known::No => return Match::NotViable,
        Known::Unknown(_) => return Match::Unranked,
    };

    let parameter_shape = the_shape_of(parameter);
    let argument_shape = the_shape_of(argument);
    if parameter_shape == argument_shape {
        return Match::Exact;
    }

    if is_a_promotion(&argument_shape, &parameter_shape) {
        return Match::Promotion;
    }

    match parse_type_spelling(&argument).convertible_to(&parse_type_spelling(parameter)) {
        Known::Yes(()) => Match::Conversion,
        Known::No => Match::NotViable,
        Known::Unknown(_) => Match::Unranked,
    }
}

/// **A type without the things a call does not choose on**: top-level `const`, a reference, and the whitespace around
/// them. `const T&` and `T` are one type as far as which overload a call picks.
fn the_shape_of(spelling: &str) -> String {
    let mut text = spelling.trim().to_string();
    // Pointers and arrays are shape: strip the reference markers only.
    loop {
        let trimmed = text.trim();
        if let Some(before) = trimmed.strip_suffix("&&").or_else(|| trimmed.strip_suffix('&')) {
            text = before.trim_end().to_string();
        } else {
            break;
        }
    }
    // A leading `const`/`volatile` on the object itself is not part of the choice.
    loop {
        let trimmed = text.trim_start();
        if let Some(after) = trimmed.strip_prefix("const ").or_else(|| trimmed.strip_prefix("volatile ")) {
            text = after.trim_start().to_string();
        } else {
            break;
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The promotions a compiler ranks above a conversion, and only those.
///
/// An **integral promotion** goes to `int` or `unsigned int` and no further: `short` to `long` is a conversion, not a
/// promotion, and the first version of this function said otherwise (it listed every wider integral type as a
/// destination) — which made `f(long)` preferred over `f(int)` for a `short`, the opposite of what the language says.
/// The other promotion is `float` to `double`.
fn is_a_promotion(from: &str, to: &str) -> bool {
    const NARROW: [&str; 6] = [
        "char",
        "signed char",
        "unsigned char",
        "short",
        "short int",
        "unsigned short",
    ];
    const TO_INT: [&str; 2] = ["int", "unsigned int"];

    (NARROW.contains(&from) && TO_INT.contains(&to)) || (from == "float" && to == "double")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_type(spelling: &str) -> Known<String> {
        Known::Yes(spelling.to_string())
    }

    fn candidate(parameters: &[&str]) -> Candidate {
        Candidate::new(
            parameters.iter().map(|parameter| (*parameter).to_string()).collect(),
            false,
            parameters.join(", "),
        )
    }

    #[test]
    fn the_integer_overload_is_chosen_for_an_integer_and_the_floating_one_for_a_floating() {
        let candidates = vec![candidate(&["int"]), candidate(&["double"])];

        assert_eq!(choose(&[a_type("int")], &candidates), Choice::One(0));
        assert_eq!(choose(&[a_type("double")], &candidates), Choice::One(1));
    }

    #[test]
    fn a_promotion_beats_a_conversion() {
        // `short` to `int` is a *promotion*; `short` to `long` is a conversion. (The first version of this test said
        // `int` to `long` was a promotion, which it is not — both of those are conversions, and the language calls the
        // call ambiguous. Written down because the mistake is an easy one to repeat.)
        let candidates = vec![candidate(&["long"]), candidate(&["int"])];
        assert_eq!(choose(&[a_type("short")], &candidates), Choice::One(1));
    }

    #[test]
    fn arity_narrows_the_set_before_any_type_is_looked_at() {
        let candidates = vec![candidate(&["int"]), candidate(&["int", "int"])];

        assert_eq!(choose(&[a_type("int")], &candidates), Choice::One(0));
        assert_eq!(choose(&[a_type("int"), a_type("int")], &candidates), Choice::One(1));
        // Both need a default argument for a zero-argument call, so both are viable and the one using fewer defaults
        // wins — see `choose`. "None" would be a claim about the program that is not true.
        assert_eq!(choose(&[], &candidates), Choice::One(0));
    }

    #[test]
    fn a_parameter_that_writes_const_and_a_reference_is_the_same_type() {
        let candidates = vec![candidate(&["const std::string&"]), candidate(&["int"])];
        assert_eq!(
            choose(&[a_type("std::string")], &candidates),
            Choice::One(0),
            "`const std::string&` and `std::string` are one type seen two ways"
        );
    }

    #[test]
    fn an_argument_with_no_type_at_all_leaves_no_candidate_viable() {
        // `Known::No` is "this expression has no type", which is the only way an argument is `NotViable`: a class whose
        // conversion this analysis cannot see answers `Unknown` instead, and must not be read as "no".
        let candidates = vec![candidate(&["int"]), candidate(&["double"])];
        assert_eq!(choose(&[Known::No], &candidates), Choice::None);
    }

    #[test]
    fn an_argument_nothing_is_known_about_makes_the_call_ambiguous_rather_than_wrong() {
        // A `std::string` **can** convert to `int` through a conversion operator, and this analysis cannot see one. The
        // answer that produces no finding is "ambiguous", not "no candidate".
        let candidates = vec![candidate(&["int"]), candidate(&["double"])];
        assert_eq!(choose(&[a_type("std::string")], &candidates), Choice::Ambiguous);
    }

    #[test]
    fn two_candidates_that_tie_everywhere_are_ambiguous_and_not_a_choice() {
        let candidates = vec![candidate(&["int"]), candidate(&["int"])];
        assert_eq!(
            choose(&[a_type("int")], &candidates),
            Choice::Ambiguous,
            "a check must say nothing rather than pick one of two identical candidates"
        );
    }

    #[test]
    fn an_unknown_argument_neither_wins_nor_loses_but_cannot_beat_a_known_exact_one() {
        let candidates = vec![candidate(&["int"]), candidate(&["double"])];
        assert_eq!(
            choose(&[Known::Unknown(crate::sema::symbol::UnknownReason::DependentName)], &candidates),
            Choice::Ambiguous,
            "nothing is known about the argument, so nothing ranks these two apart"
        );
        assert_eq!(
            choose(&[a_type("double")], &candidates),
            Choice::One(1),
            "and a known argument still decides"
        );
    }

    #[test]
    fn an_ordinary_function_beats_a_template_that_is_equally_good() {
        // The language's rule: a tie between a template and an ordinary function goes to the ordinary one. Both are
        // `Exact` for an `int` argument — the template after its deduction, which the caller does, not this module.
        let candidates = vec![
            candidate(&["T"]).as_a_template(),
            candidate(&["int"]),
        ];
        assert_eq!(
            choose(&[a_type("int")], &candidates),
            Choice::One(1),
            "`f(int)` is what `f(1)` means when `f(T)` is in view too"
        );

        // Two templates are still a tie, and a tie is still ambiguous: the rule above is about the *ordinary* one.
        let both = vec![candidate(&["T"]).as_a_template(), candidate(&["T"]).as_a_template()];
        assert_eq!(choose(&[a_type("int")], &both), Choice::Ambiguous);
    }

    #[test]
    fn a_more_parameterful_candidate_stays_viable_because_defaults_are_not_in_the_spelling() {
        let candidates = vec![candidate(&["int"]), candidate(&["int", "int"])];
        assert_eq!(
            choose(&[a_type("int")], &candidates),
            Choice::One(0),
            "the one that needs no default wins, but both were viable"
        );
    }
}

/// **A parameter's type without its name.**
///
/// `int n` and `const char* s` are how parameters are written, and a type comparison has to see `int` and
/// `const char*`. The name is the last identifier when what precedes it is a type — a `const char* s` keeps its `*`,
/// which is before the name. A heuristic, stated as one: a parameter this cannot separate keeps its whole spelling, and
/// the resolver reads the pair as `Match::Unranked`, which is the quiet direction.
pub(crate) fn a_type_without_its_name(parameter: &str) -> String {
    let parameter = parameter.split('=').next().unwrap_or(parameter).trim();
    let Some(last) = parameter.split_whitespace().last() else {
        return parameter.to_string();
    };
    if last == parameter {
        return parameter.to_string();
    }
    let is_a_name = last
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
        && last
            .chars()
            .next()
            .is_some_and(|character| character.is_alphabetic() || character == '_');
    if is_a_name {
        return parameter[..parameter.len() - last.len()].trim_end().to_string();
    }
    parameter.to_string()
}

/// **The parameter types in a `parameter_list` spelling**, and whether it ends in `...`.
///
/// `(int a, const char* b = 0)` is `["int", "const char*"]`; `(Args&&... args)` is `([], true)`. The split is at a
/// comma at depth zero, because `std::pair<int, int>` is one parameter; the default and the parameter's own name are
/// dropped by [`a_type_without_its_name`]. A spelling whose parentheses are unbalanced answers what it could read —
/// a caller that cannot parse a declaration is a caller that should rank it as `Unranked`, not that should panic.
pub fn parameters_in(parameter_list: &str) -> (Vec<String>, bool) {
    let inner = parameter_list
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');

    let mut parameters = Vec::new();
    let mut variadic = false;
    let mut depth = 0i32;
    let mut current = String::new();
    let take = |parameter: &str, parameters: &mut Vec<String>, variadic: &mut bool| {
        let parameter = parameter.trim();
        if parameter.is_empty() {
            return;
        }
        let (kind, rest) = match parameter.split_once(' ') {
            Some((kind, rest)) => (kind, rest),
            None => (parameter, ""),
        };
        if kind == "..." || rest.contains("...") || parameter.contains("...") {
            *variadic = true;
            return;
        }
        parameters.push(a_type_without_its_name(parameter));
    };

    for character in inner.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                take(&current, &mut parameters, &mut variadic);
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    take(&current, &mut parameters, &mut variadic);

    (parameters, variadic)
}

/// **Is `mine` more specialized than `theirs`?** — the language's partial ordering of two function templates.
///
/// Decided by **deducing one declaration's parameters from the other's spellings**, with no argument type anywhere: if
/// `theirs`'s parameters can be deduced against `mine`'s but not the other way round, `mine` is the more specialized.
/// `T*` beats `T` for that reason, and `vector<T>` beats `T`; two declarations that deduce into each other equally are a
/// tie, and a tie stays ambiguous.
pub(crate) fn more_specialized(mine: &Candidate, theirs: &Candidate) -> bool {
    let deduces = |from: &Candidate, to: &Candidate| -> bool {
        from.template_parameters.is_empty()
            || to.template_parameters.is_empty()
            || to.declared.is_empty()
            || from.declared.is_empty()
            || from.declared.len() != to.declared.len()
            || from
                .declared
                .iter()
                .zip(&to.declared)
                .all(|(from_spelling, to_spelling)| {
                    crate::sema::deduction::deduce(
                        &from.template_parameters,
                        std::slice::from_ref(from_spelling),
                        &[crate::Known::Yes(to_spelling.clone())],
                    )
                    .is_some()
                })
    };

    let theirs_deduces_into_mine = deduces(theirs, mine);
    let mine_deduces_into_theirs = deduces(mine, theirs);
    theirs_deduces_into_mine && !mine_deduces_into_theirs
}

    #[test]
    fn the_more_specialized_of_two_templates_wins() {
        // The shape that needed it: `take(T*)` and `take(T)` both deduce to `int*` for an `int*` argument, so the
        // ranking ties and the language's answer is partial ordering — `T*` is more specialized.
        let by_pointer = Candidate::new(vec!["int*".to_string()], false, "take(T*)".to_string())
            .with_declaration(vec!["T*".to_string()], vec!["T".to_string()]);
        let by_value = Candidate::new(vec!["int*".to_string()], false, "take(T)".to_string())
            .with_declaration(vec!["T".to_string()], vec!["T".to_string()]);

        assert!(
            more_specialized(&by_pointer, &by_value),
            "`T*` is more specialized than `T`"
        );
        assert!(
            !more_specialized(&by_value, &by_pointer),
            "and the relation is not symmetric"
        );

        let chosen = choose(
            &[crate::Known::Yes("int*".to_string())],
            &[
                by_pointer.clone(),
                by_value.clone(),
            ],
        );
        assert_eq!(chosen, Choice::One(0), "so the pointer template is the one a call means");

        // Two declarations that are the same are neither more specialized, and a tie is still a tie.
        assert!(!more_specialized(&by_value, &by_value));
        assert_eq!(
            choose(&[crate::Known::Yes("int*".to_string())], &[by_value.clone(), by_value.clone()]),
            Choice::Ambiguous
        );
    }