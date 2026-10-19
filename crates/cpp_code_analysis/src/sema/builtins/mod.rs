//! **The compiler's own builtins, as far as this layer can honestly read them.**
//!
//! # Why this exists
//!
//! MSVC's STL is written in terms of intrinsics the compiler answers about types:
//!
//! ```text
//! template <class _Ty, class... _Args>
//! inline constexpr bool is_constructible_v = __is_constructible(_Ty, _Args...);
//! ```
//!
//! and the interesting uses are the ones that *decide* something: `enable_if_t<__is_same(_Ty, _Uty), int>` has one
//! definition for `true` and none for `false`, so what the analysis is being asked is not "what is the type of
//! `__is_same(A, B)`" but "**is it true**".
//!
//! Nothing here evaluates C++. What it does is narrower and worth stating exactly: a builtin is recognised by name,
//! and the ones whose answer follows from the **types the call was written with** are decided. `__is_pointer<int*>`
//! is `true` — the analysis models pointers — and `__is_constructible<T, U>` is not decided, because that is a
//! question about a class body and half of one is not in front of this layer. An undecided builtin still has a
//! **type**, which is `bool`, and answering that is the difference between "a bool whose value I do not know" and
//! "a class called `__is_constructible`".
//!
//! # The shape of an answer
//!
//! [`Builtin::answer`] is the whole interface. [`Answer::Boolean`] carries the value only when it was decided — a
//! `bool` spelling that is *not* decided stays `None` rather than becoming a coin flip, which is the direction the
//! rest of this crate fails in. [`Answer::Unknown`] is the honest answer for the long tail of intrinsics whose
//! meaning is a compiler's business (`__builtin_memcpy`, `__crt_va_start`, `__iso_volatile_load32`): they are
//! recognised so that they are not mistaken for ordinary functions, and nothing is claimed about them.
//!
//! # Files
//!
//! * [`traits`] — the `__is_*` family: what each one asks, and which of them the written arguments decide.
//! * [`transforms`] — the builtins that *are* a type rather than a question about one.

pub mod traits;
pub mod transforms;

/// **What a builtin name means**, as this layer is willing to answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// A `bool` whose value is known: `__is_pointer<int*>` is `true`.
    ///
    /// The value is what makes this worth having. A template argument is compared as a **spelling** where a partial
    /// specialization is chosen (`enable_if<true, _Ty>` is a pattern), so a builtin that folds to `true` is what lets
    /// that pattern match.
    Boolean(bool),
    /// A `bool` whose value is not decided: the type is `bool` and the question is left open.
    UndecidedBoolean,
    /// A builtin that produces a type: `__decay(x)` is `int` for an `int&`.
    Type(crate::sema::types::Type),
    /// A name this layer knows is a builtin and will not answer for.
    Unmodelled,
}

/// **What is known about one builtin name.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    /// The name as the compiler spells it, with the leading underscores.
    pub name: &'static str,
    /// How many arguments the builtin takes when written as a call — `None` for a variadic one.
    pub arity: Option<usize>,
    /// What kind of thing it is, which is what decides whose question it is.
    pub kind: Kind,
}

/// **Which family a builtin belongs to.** The families are separate files because they are separate questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An `__is_*`: a question about types, answered `bool`.
    Trait(traits::Trait),
    /// A builtin that **is** a type, given its arguments.
    Transform(transforms::Transform),
    /// Recognised and not modelled.
    Unmodelled,
}

/// **The builtin with this name**, if it is one.
///
/// A linear scan of two tables that fit in a cache line, called once per spelling that reaches the type reader with
/// an underscore in it — which is a rare enough shape that a map would cost more to build than it saves.
pub fn named(name: &str) -> Option<Builtin> {
    if let Some(found) = traits::named(name) {
        return Some(found);
    }

    transforms::named(name)
}

/// **What this builtin answers for these written arguments.**
///
/// The arguments are the spellings inside its parentheses or angle brackets, already split — see
/// [`split_arguments`]. `None` is "not one of ours", and the two `bool` answers are told apart on purpose: an
/// undecided `bool` still has a type, and a caller that only wants the type should not have to know which it got.
pub fn answer(builtin: Builtin, arguments: &[String]) -> Answer {
    match builtin.kind {
        Kind::Trait(question) => match traits::decide(question, arguments) {
            Some(value) => Answer::Boolean(value),
            None => Answer::UndecidedBoolean,
        },
        Kind::Transform(transform) => transforms::apply(transform, arguments),
        Kind::Unmodelled => Answer::Unmodelled,
    }
}

/// **The arguments of a builtin written in a spelling**, and the name in front of them.
///
/// Recognises the two forms the intrinsics are written in — `__is_same(A, B)` as a call and
/// `__type_pack_element<0, _Types...>` as a template-id — because MSVC's headers use both and the difference between
/// them is punctuation rather than meaning. `None` for a spelling that is not a builtin call at all, which is the
/// ordinary case and the one this must not slow down.
///
/// The split is at **depth zero**: `__is_same(_Ty, vector<int>)` has two arguments, and a splitter that cut at every
/// comma would hand over three.
pub fn split_a_builtin_spelling(written: &str) -> Option<(Builtin, Vec<String>)> {
    let written = written.trim();
    let open = written.find(['(', '<'])?;
    let closer = if written.as_bytes().get(open) == Some(&b'(') {
        ')'
    } else {
        '>'
    };
    if !written.ends_with(closer) {
        return None;
    }

    let builtin = named(written[..open].trim())?;
    let inner = &written[open + 1..written.len() - 1];

    Some((builtin, split_arguments(inner)))
}

/// **The parts of an argument list, split where the language splits them.**
///
/// A comma at depth zero separates arguments; every other comma belongs to something inside one of them —
/// `pair<int, int>`, `int(*)(int, int)`, `(a, b)`. Angle brackets count as depth here, which is the choice that makes
/// `__is_same(vector<int>, vector<int>)` two arguments rather than three.
pub fn split_arguments(written: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;

    for character in written.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }

        current.push(character);
    }

    let last = current.trim();
    if !last.is_empty() || !parts.is_empty() {
        parts.push(last.to_string());
    }

    parts
}
/// **A written argument as the pattern comparison should see it**: a decidable builtin, folded to what it answers.
///
/// `enable_if<true, _Ty>` is a class pattern with a **literal** in its first slot, and the uses that select it are
/// written `enable_if<__is_same(_Ty, _Uty), _Ty>` — so a comparison made on spellings has to see `true` where the
/// file wrote the intrinsic. This is the whole of that step, and it is a pure function of a spelling, which is what
/// lets it be tested without a session, a file or an index.
///
/// An undecided builtin — `__is_constructible(T, U)` — comes back **unchanged**, and so does anything that is not a
/// builtin at all. That is what keeps the fold from inventing a match: it can only turn a spelling into the value its
/// own arguments settled, and a question they did not settle stays unequal to everything but itself.
pub fn folded(written: &str) -> String {
    let Some((builtin, arguments)) = split_a_builtin_spelling(written) else {
        return written.to_string();
    };

    match answer(builtin, &arguments) {
        Answer::Boolean(value) => value.to_string(),
        _ => written.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decided_builtin_folds_to_the_word_a_pattern_is_written_with() {
        assert_eq!(folded("__is_same(int, int)"), "true");
        assert_eq!(folded("__is_same(int, double)"), "false");
        assert_eq!(folded("__is_pointer(int*)"), "true");
        assert_eq!(folded("__is_pointer(int)"), "false");
    }

    #[test]
    fn an_undecided_builtin_and_an_ordinary_type_both_come_back_unchanged() {
        // The direction that matters: a question the written arguments do not settle must not become a `false` that
        // makes `enable_if<false, _Ty>` match.
        for unchanged in [
            "__is_constructible(_Ty, _Args...)",
            "__is_pointer(_Ty)",
            "__is_pointer(Widget)",
            "vector<int>",
            "int",
        ] {
            assert_eq!(folded(unchanged), unchanged, "this one is not decided");
        }
    }

    #[test]
    fn the_two_forms_a_builtin_is_written_in_are_both_read() {
        assert_eq!(
            split_a_builtin_spelling("__is_pointer(int*)")
                .map(|(builtin, arguments)| (builtin.name, arguments)),
            Some(("__is_pointer", vec!["int*".to_string()]))
        );
        assert_eq!(
            split_a_builtin_spelling("__type_pack_element<0, _Types...>")
                .map(|(builtin, arguments)| (builtin.name, arguments)),
            Some((
                "__type_pack_element",
                vec!["0".to_string(), "_Types...".to_string()]
            )),
            "a template-id form is split the same way a call is"
        );
        assert!(
            split_a_builtin_spelling("vector<int>").is_none(),
            "an ordinary class is not a builtin"
        );
    }

    #[test]
    fn arguments_are_split_where_the_language_splits_them() {
        assert_eq!(split_arguments("A, B"), vec!["A", "B"]);
        assert_eq!(
            split_arguments("vector<int>, pair<int, int>"),
            vec!["vector<int>", "pair<int, int>"]
        );
        assert_eq!(split_arguments(""), Vec::<String>::new());
    }
}
