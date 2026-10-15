//! **The builtins that are a type rather than a question about one.**
//!
//! # Why this file is nearly empty, on purpose
//!
//! Measured before it was written: over MSVC 14.35's `type_traits`, `xutility` and the compiler's own headers, the
//! type-producing intrinsics are **rare** — `__underlying_type` appears once and `__make_integer_seq` once — while the
//! `__is_*` family appears hundreds of times (see [`super::traits`]). The STL does its type arithmetic in templates
//! (`remove_reference`, `conditional`, `enable_if`), not in intrinsics, so a table of transforms would be a table of
//! guesses about a shape the headers barely use.
//!
//! What is here is therefore the list of names that **are** real, each answering [`Answer::Unmodelled`]: recognised,
//! so that a spelling like `__underlying_type(_Ty)` is not read as a class of that name, and not claimed, because
//! answering it means looking up an enum's declaration and this layer is holding a spelling.
//!
//! # What a real transform would look like
//!
//! The shape is ready for one: [`apply`] takes the written arguments and answers an [`Answer`], and the type algebra
//! it needs already exists (`Type::Pointer`, `Type::Reference`, `Type::Array`, and the pack variant). The two names
//! below are the ones that would have to be answered by something *other* than the algebra — an enum's underlying
//! type is a fact about a declaration, and `__make_integer_seq` builds a sequence this layer has no spelling for —
//! which is exactly why they are the two left unmodelled rather than the two that were hard to reach.

use super::{Answer, Builtin, Kind};

/// **A builtin whose answer is a type.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// `__underlying_type(E)`: the integral type an enumeration is represented by. A fact about the enum's
    /// declaration, which is in the index and not in the spelling.
    UnderlyingType,
    /// `__make_integer_seq<template, T, N>`: the `integer_sequence` a standard header builds its index packs with.
    MakeIntegerSeq,
    /// `__type_pack_element<I, Ts...>`: the `I`th of the types written after it — `<tuple>` and `<utility>` reach for
    /// it whenever they need one element of a pack by position.
    ///
    /// **This one is answered when the index is a literal**, which is how the headers write it: the arguments after
    /// the index are the pack, spelled out, and the answer is the one at that position. A non-literal index — a
    /// template parameter, `__type_pack_element<_Idx, _Types...>` — is not decided, and neither is an index past the
    /// end of what was written, because that is a header this layer is misreading rather than a type.
    TypePackElement,
}

/// **The transforms this layer knows by name**, all of them unmodelled — see the module documentation for why that
/// is the answer rather than a gap.
const TRANSFORMS: &[(&str, Transform)] = &[
    ("__underlying_type", Transform::UnderlyingType),
    ("__make_integer_seq", Transform::MakeIntegerSeq),
    ("__type_pack_element", Transform::TypePackElement),
];

/// **The transform with this name**, wrapped as a builtin.
pub fn named(name: &str) -> Option<Builtin> {
    TRANSFORMS
        .iter()
        .find(|(spelling, _)| *spelling == name)
        .map(|(spelling, transform)| Builtin {
            name: spelling,
            arity: None,
            kind: Kind::Transform(*transform),
        })
}

/// **What this transform produces for these written arguments** — which is, for both of them, nothing this layer
/// will claim. See the module documentation.
pub fn apply(transform: Transform, arguments: &[String]) -> Answer {
    match transform {
        // **The one transform whose answer is in the arguments themselves.** `__type_pack_element<1, int, double>`
        // is `double`: the index is written as a literal and the pack is spelled out beside it, so nothing has to be
        // looked up. Everything else about this builtin — an index that is a template parameter, a pack that is a
        // parameter pack — is a question about an instantiation, and that is not here.
        Transform::TypePackElement => {
            let Some((index, rest)) = arguments.split_first() else {
                return Answer::Unmodelled;
            };
            let Ok(index) = index.trim().parse::<usize>() else {
                return Answer::Unmodelled;
            };
            match rest.get(index) {
                Some(written) => Answer::Type(crate::sema::types::parse_type_spelling(written)),
                None => Answer::Unmodelled,
            }
        }
        // A fact about an enum's declaration, and a sequence this layer has no spelling for. See the module
        // documentation for why these two are the ones left unmodelled.
        Transform::UnderlyingType | Transform::MakeIntegerSeq => Answer::Unmodelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_real_transforms_are_recognised_and_not_answered_for() {
        for (spelling, _) in TRANSFORMS {
            let builtin = named(spelling).expect("the name is in the table");
            assert_eq!(builtin.kind, Kind::Transform(TRANSFORMS.iter().find(|(name, _)| name == spelling).expect("just found").1));
            assert_eq!(apply(Transform::UnderlyingType, &["E".to_string()]), Answer::Unmodelled);
        }

        assert!(named("__decay").is_none(), "an expression builtin is not a type-producing one");
    }
    /// **The pack element a literal index names is that type.**
    ///
    /// This is the one type-producing builtin whose answer is in its own arguments: `<tuple>` writes
    /// `__type_pack_element<1, _Types...>` and when the pack beside it is spelled out the answer needs no lookup.
    /// The two refusals matter as much: an index that is a template parameter is a question about an instantiation,
    /// and an index past the end of what was written is a header this layer is misreading — neither is a type.
    #[test]
    fn the_pack_element_a_literal_index_names_is_that_type() {
        let arguments = |parts: &[&str]| parts.iter().map(|one| one.to_string()).collect::<Vec<String>>();

        assert_eq!(
            apply(Transform::TypePackElement, &arguments(&["1", "int", "double"])),
            Answer::Type(crate::sema::types::parse_type_spelling("double"))
        );
        assert_eq!(
            apply(Transform::TypePackElement, &arguments(&["0", "const int*"])),
            Answer::Type(crate::sema::types::parse_type_spelling("const int*")),
            "and the element keeps its own spelling, qualifiers and all"
        );
        assert_eq!(apply(Transform::TypePackElement, &arguments(&["_Idx", "int"])), Answer::Unmodelled);
        assert_eq!(apply(Transform::TypePackElement, &arguments(&["4", "int"])), Answer::Unmodelled);
        assert_eq!(apply(Transform::TypePackElement, &arguments(&[])), Answer::Unmodelled);
    }
}


