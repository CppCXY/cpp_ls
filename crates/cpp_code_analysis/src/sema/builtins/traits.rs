//! **The `__is_*` family** — a question about types, answered `bool`.
//!
//! # What is decided, and what is not
//!
//! The rule for deciding is the one the rest of the crate uses: an answer only when the **written arguments** make it
//! certain.
//!
//! ```text
//! __is_pointer(int*)          true       the analysis models pointers
//! __is_pointer(int)           false      an `int` is not a pointer, and nothing can change that
//! __is_pointer(_Ty)           unknown    `_Ty` is a parameter: it could be one
//! __is_pointer(Widget)        unknown    `Widget` may be a typedef for a pointer
//! __is_same(int, double)      false      two builtins with different names are different types
//! __is_same(int, Widget)      unknown    `Widget` could be a typedef for `int`
//! __is_same(vector<int>, vector<int>)  true
//! __is_constructible(T, U)    unknown    a question about a class body, and half of one is not in front of this layer
//! ```
//!
//! The last line is the honest and the common case: most of the traits MSVC's STL asks about are questions about a
//! **class**, and answering `false` because the class is not in front of us is exactly the wrong direction — it makes
//! `enable_if<false, _Ty>` match and the analysis then believes it has a type it does not have. `None` keeps the
//! question open, which the pattern matching reads as "this candidate is not ruled out" rather than "this candidate
//! is right" — see the note on `DeclFact::pattern`.

use super::{Answer, Builtin, Kind};

/// **Which question a trait asks.** One variant per question this layer can tell apart, because two intrinsics that
/// ask the same thing must be decided by the same rule — MSVC spells the same question both `__is_pointer` and
/// `is_pointer` and the library's own `is_pointer_v` is built on the intrinsic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trait {
    /// Is this a pointer? `__is_pointer`.
    Pointer,
    /// Is this an lvalue reference? `__is_lvalue_reference`.
    LvalueReference,
    /// Is this an rvalue reference? `__is_rvalue_reference`.
    RvalueReference,
    /// Is this a reference of either kind? `__is_reference`.
    Reference,
    /// Is this an array? `__is_array`, and whether it has a bound.
    Array(ArrayBound),
    /// Is this `void`? `__is_void`.
    Void,
    /// Is this an integral type? `__is_integral`.
    Integral,
    /// Is this a floating point type? `__is_floating_point`.
    FloatingPoint,
    /// Is this a class, a union, a function, an enum … — a question about a declaration this layer cannot see.
    Unseen,
    /// Are these two the same type? `__is_same`.
    Same,
    /// Anything else the compiler asks about a class body, an object's lifetime, or a conversion.
    Unknown,
}

/// **Whether an array's bound is part of the question.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayBound {
    /// Any array at all: `__is_array`.
    Either,
    /// An array with a bound: `int[4]`.
    Bounded,
    /// An array without one: `int[]`.
    Unbounded,
}

/// The names are the ones the **compiler** defines — the `__`-prefixed family — and deliberately not the library's
/// own `is_same` / `is_pointer` / `has_virtual_destructor`. In a *type* position those are **class templates**: the
/// reader meets `std::is_same<_Ty, _Uty>` as a spelling and the answer is a class, while what asks the compiler is
/// the intrinsic beside it and the library's `is_same_v` is a `constexpr bool` built on it. Listing both spellings
/// would have made `std::is_same<A, B>` read as `bool`, which is the sort of wrong answer that reads like a right
/// one — so the table holds the compiler's names and the library's are left to the library.
/// **The traits this layer knows by name**, and the question each one asks.
///
/// The names are the ones a compiler actually defines and the STL actually writes, taken from the headers this crate
/// is read against — `type_traits`, `xutility` and the compiler's own `vcruntime`. Both spellings of each are listed
/// where the library has both, because a lookup that misses one falls through to "an unknown class" and that is the
/// state this module exists to end.
const TRAITS: &[(&str, Trait)] = &[
    ("__is_pointer", Trait::Pointer),
    ("__is_lvalue_reference", Trait::LvalueReference),
    ("__is_rvalue_reference", Trait::RvalueReference),
    ("__is_reference", Trait::Reference),
    ("__is_array", Trait::Array(ArrayBound::Either)),
    ("__is_bounded_array", Trait::Array(ArrayBound::Bounded)),
    ("__is_unbounded_array", Trait::Array(ArrayBound::Unbounded)),
    ("__is_void", Trait::Void),
    ("__is_integral", Trait::Integral),
    ("__is_floating_point", Trait::FloatingPoint),
    ("__is_same", Trait::Same),
    ("__is_enum", Trait::Unseen),
    ("__is_union", Trait::Unseen),
    ("__is_class", Trait::Unseen),
    ("__is_function", Trait::Unseen),
    ("__is_member_object_pointer", Trait::Unseen),
    ("__is_member_function_pointer", Trait::Unseen),
    ("__is_member_pointer", Trait::Unseen),
    ("__is_constructible", Trait::Unknown),
    ("__is_trivially_constructible", Trait::Unknown),
    ("__is_nothrow_constructible", Trait::Unknown),
    ("__is_default_constructible", Trait::Unknown),
    ("__is_copy_constructible", Trait::Unknown),
    ("__is_move_constructible", Trait::Unknown),
    ("__is_assignable", Trait::Unknown),
    ("__is_assignable_no_precondition_check", Trait::Unknown),
    ("__is_trivially_assignable", Trait::Unknown),
    ("__is_nothrow_assignable", Trait::Unknown),
    ("__is_destructible", Trait::Unknown),
    ("__is_trivially_destructible", Trait::Unknown),
    ("__is_nothrow_destructible", Trait::Unknown),
    ("__is_trivially_copyable", Trait::Unknown),
    ("__is_trivial", Trait::Unknown),
    ("__is_standard_layout", Trait::Unknown),
    ("__is_empty", Trait::Unknown),
    ("__is_polymorphic", Trait::Unknown),
    ("__is_abstract", Trait::Unknown),
    ("__is_final", Trait::Unknown),
    ("__is_aggregate", Trait::Unknown),
    ("__is_pod", Trait::Unknown),
    ("__is_base_of", Trait::Unknown),
    ("__is_pointer_interconvertible_base_of", Trait::Unknown),
    ("__is_convertible_to", Trait::Unknown),
    ("__is_nothrow_convertible", Trait::Unknown),
    ("__is_invocable", Trait::Unknown),
    ("__is_nothrow_invocable", Trait::Unknown),
    ("__has_virtual_destructor", Trait::Unknown),
    ("__is_swappable", Trait::Unknown),
    ("__is_nothrow_swappable", Trait::Unknown),
    ("__is_trivially_relocatable", Trait::Unknown),
    ("__is_scoped_enum", Trait::Unknown),
    ("__is_nothrow_copy_constructible", Trait::Unknown),
    ("__is_nothrow_move_constructible", Trait::Unknown),
    ("__is_trivially_copy_constructible", Trait::Unknown),
    ("__is_trivially_move_constructible", Trait::Unknown),
    ("__is_nothrow_copy_assignable", Trait::Unknown),
    ("__is_nothrow_move_assignable", Trait::Unknown),
    ("__is_trivially_copy_assignable", Trait::Unknown),
    ("__is_trivially_move_assignable", Trait::Unknown),
    ("__is_signed", Trait::Unknown),
    ("__is_unsigned", Trait::Unknown),
];

/// **The trait with this name**, wrapped as a builtin.
pub fn named(name: &str) -> Option<Builtin> {
    TRAITS
        .iter()
        .find(|(spelling, _)| *spelling == name)
        .map(|(spelling, question)| Builtin {
            name: spelling,
            arity: None,
            kind: Kind::Trait(*question),
        })
}

/// **What this trait answers for these written arguments** — `None` when the answer is not certain.
///
/// One argument for the unary questions, two for [`Trait::Same`], and every other shape left open: a builtin written
/// with the wrong number of arguments is a header this layer is misreading, and guessing at it is how a wrong type
/// gets into an answer that nothing downstream can tell from a right one.
pub fn decide(question: Trait, arguments: &[String]) -> Option<bool> {
    match question {
        Trait::Pointer => shape_of(arguments)?.is_pointer(),
        Trait::LvalueReference => shape_of(arguments)?.is_reference(false),
        Trait::RvalueReference => shape_of(arguments)?.is_reference(true),
        Trait::Reference => match shape_of(arguments)? {
            Shape::Reference { .. } => Some(true),
            Shape::Open => None,
            _ => Some(false),
        },
        Trait::Array(bound) => match shape_of(arguments)? {
            Shape::Array(extent) => Some(match bound {
                ArrayBound::Either => true,
                ArrayBound::Bounded => extent,
                ArrayBound::Unbounded => !extent,
            }),
            Shape::Open => None,
            _ => Some(false),
        },
        Trait::Void => match shape_of(arguments)? {
            Shape::Builtin(name) => Some(name == "void"),
            Shape::Open => None,
            _ => Some(false),
        },
        Trait::Integral => builtin_kind(arguments, is_an_integral_spelling),
        Trait::FloatingPoint => builtin_kind(arguments, is_a_floating_spelling),
        Trait::Same => same(arguments),
        // A question about a class body, an object's lifetime, or a conversion. Neither `true` nor `false` is
        // certain from a spelling, so neither is given.
        Trait::Unseen | Trait::Unknown => None,
    }
}

/// **What one written argument is, as far as its shape settles it.**
///
/// The distinction that matters: every variant but [`Shape::Open`] is a type whose **kind** the spelling has already
/// decided — a pointer is a pointer whatever it points at, and `unsigned long long` is integral whatever a typedef
/// with the same spelling elsewhere might mean — while `Open` is a name or a template parameter, which could be a
/// typedef for anything at all. Those are the two answers this module can give: a fact read off the shape, or `None`.
enum Shape<'a> {
    Pointer,
    /// A reference, and which kind — the two traits that ask about references differ only here.
    Reference { rvalue: bool },
    /// An array, with whether it was written with a bound.
    Array(bool),
    Function,
    /// A builtin, by the name the file spelled.
    Builtin(&'a str),
    /// A name, a template parameter or a pack.
    Open,
}

impl Shape<'_> {
    /// `open` is what to answer for a name; every other shape is a fact about the kind.
    fn is_pointer(&self) -> Option<bool> {
        match self {
            Shape::Pointer => Some(true),
            Shape::Open => None,
            _ => Some(false),
        }
    }

    fn is_reference(&self, rvalue: bool) -> Option<bool> {
        match self {
            Shape::Reference { rvalue: written } => Some(*written == rvalue),
            Shape::Open => None,
            _ => Some(false),
        }
    }
}

/// **The shape of the one argument a unary trait was written with**, or `None` when it was not written that way.
fn shape_of(arguments: &[String]) -> Option<Shape<'_>> {
    let [only] = arguments else {
        return None;
    };
    Some(shape(only))
}

/// **Where a trait that asks about a builtin's kind gets its answer** — and it is only a builtin's kind that is
/// settled by the spelling. A name is a name: `vector<int>` is neither integral nor floating, and this layer cannot
/// know that without the class.
fn builtin_kind(arguments: &[String], belongs: fn(&str) -> bool) -> Option<bool> {
    match shape_of(arguments)? {
        Shape::Builtin(name) => Some(belongs(name)),
        Shape::Open => None,
        _ => Some(false),
    }
}

/// **Are these two the same type?**
///
/// `true` when the two spellings parse to one type, which is the answer that lets a pattern match. `false` only when
/// both are builtins with different names, or when one is a builtin and the other is a pointer, a reference or an
/// array — a builtin is none of those, so the shapes alone settle it. Everything else is `None`, because a typedef is
/// a spelling that means something this layer has not looked up: `__is_same(int, size_type)` is very often `true`.
fn same(arguments: &[String]) -> Option<bool> {
    let [left, right] = arguments else {
        return None;
    };

    let (left_type, right_type) = (
        crate::sema::types::parse_type_spelling(left),
        crate::sema::types::parse_type_spelling(right),
    );
    if left_type == right_type {
        return Some(true);
    }

    match (shape(left), shape(right)) {
        (Shape::Builtin(one), Shape::Builtin(other)) => Some(one == other),
        (Shape::Builtin(_), other) | (other, Shape::Builtin(_)) if !matches!(other, Shape::Open) => Some(false),
        _ => None,
    }
}

/// **The shape of one written argument.**
fn shape(written: &str) -> Shape<'_> {
    let written = written.trim();
    match crate::sema::types::parse_type_spelling(written) {
        crate::sema::types::Type::Pointer { .. } => Shape::Pointer,
        crate::sema::types::Type::Reference { rvalue, .. } => Shape::Reference { rvalue },
        crate::sema::types::Type::Array { extent, .. } => Shape::Array(extent.is_some()),
        crate::sema::types::Type::Function { .. } => Shape::Function,
        // The name a builtin was written with, read back off the spelling rather than off the parsed type: the
        // parsed type of `void` carries the same word, and this keeps the two spellings' agreement obvious.
        crate::sema::types::Type::Builtin { .. } => Shape::Builtin(written),
        _ => Shape::Open,
    }
}

/// **Is this spelling one of the integral types?**
///
/// By name, because there is no other evidence in a spelling: `int`, `unsigned long long`, `char8_t` and `bool` are
/// integral and `float`, `double`, `void` and `decltype(auto)` are not. A typedef is a name this does not recognise,
/// and [`builtin_kind`] answers `None` for it rather than `false`.
fn is_an_integral_spelling(name: &str) -> bool {
    let name = name.trim();
    const INTEGRAL: &[&str] = &[
        "bool", "char", "signed char", "unsigned char", "wchar_t", "char8_t", "char16_t", "char32_t", "short",
        "short int", "unsigned short", "unsigned short int", "int", "signed", "signed int", "unsigned",
        "unsigned int", "long", "long int", "unsigned long", "unsigned long int", "long long", "long long int",
        "unsigned long long", "unsigned long long int", "__int8", "__int16", "__int32", "__int64",
        "unsigned __int8", "unsigned __int16", "unsigned __int32", "unsigned __int64", "__wchar_t",
    ];
    INTEGRAL.contains(&name)
}

/// **Is this spelling one of the floating point types?**
fn is_a_floating_spelling(name: &str) -> bool {
    let name = name.trim();
    matches!(name, "float" | "double" | "long double")
}

/// **A trait this module did not decide is still a `bool`.** See [`Answer`] — this is the one place the distinction
/// between "undecided" and "not a builtin" is expressed, and it is the reason a spelling like
/// `__is_constructible(_Ty, _Args...)` no longer reads as a class of that name.
pub fn answer_for(name: &str) -> Option<Answer> {
    let builtin = named(name)?;
    Some(match builtin.kind {
        Kind::Trait(_) => Answer::UndecidedBoolean,
        _ => Answer::Unmodelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided(question: Trait, arguments: &[&str]) -> Option<bool> {
        let arguments: Vec<String> = arguments.iter().map(|one| one.to_string()).collect();
        decide(question, &arguments)
    }

    #[test]
    fn a_pointer_is_decided_by_its_shape_and_not_by_what_it_points_at() {
        assert_eq!(decided(Trait::Pointer, &["int*"]), Some(true));
        assert_eq!(decided(Trait::Pointer, &["int"]), Some(false));
        assert_eq!(decided(Trait::Pointer, &["int[4]"]), Some(false));
        // `_Ty` is a parameter: it could be a pointer, so nothing is claimed.
        assert_eq!(decided(Trait::Pointer, &["_Ty"]), None);
        assert_eq!(decided(Trait::Pointer, &["Widget"]), None);
    }

    #[test]
    fn the_reference_questions_tell_the_two_kinds_apart() {
        assert_eq!(decided(Trait::LvalueReference, &["int&"]), Some(true));
        assert_eq!(decided(Trait::LvalueReference, &["int&&"]), Some(false));
        assert_eq!(decided(Trait::RvalueReference, &["int&&"]), Some(true));
        assert_eq!(decided(Trait::Reference, &["int&&"]), Some(true));
        assert_eq!(decided(Trait::Reference, &["int"]), Some(false));
    }

    #[test]
    fn an_array_answers_the_bound_question_as_well_as_the_array_one() {
        assert_eq!(decided(Trait::Array(ArrayBound::Either), &["int[4]"]), Some(true));
        assert_eq!(decided(Trait::Array(ArrayBound::Bounded), &["int[4]"]), Some(true));
        assert_eq!(decided(Trait::Array(ArrayBound::Bounded), &["int[]"]), Some(false));
        assert_eq!(decided(Trait::Array(ArrayBound::Unbounded), &["int[]"]), Some(true));
        assert_eq!(decided(Trait::Array(ArrayBound::Either), &["int"]), Some(false));
        assert_eq!(decided(Trait::Array(ArrayBound::Either), &["_Ty"]), None);
    }

    #[test]
    fn a_builtin_is_its_own_answer_and_a_name_is_not_one() {
        assert_eq!(decided(Trait::Void, &["void"]), Some(true));
        assert_eq!(decided(Trait::Void, &["int"]), Some(false));
        assert_eq!(decided(Trait::Integral, &["unsigned long long"]), Some(true));
        assert_eq!(decided(Trait::Integral, &["double"]), Some(false));
        assert_eq!(decided(Trait::FloatingPoint, &["double"]), Some(true));
        assert_eq!(decided(Trait::FloatingPoint, &["int"]), Some(false));
        // A typedef is not "not an integral type" — it is a question this cannot answer.
        assert_eq!(decided(Trait::Integral, &["size_type"]), None);
    }

    #[test]
    fn sameness_is_answered_only_when_the_shapes_settle_it() {
        assert_eq!(decided(Trait::Same, &["int", "int"]), Some(true));
        assert_eq!(decided(Trait::Same, &["vector<int>", "vector<int>"]), Some(true));
        assert_eq!(decided(Trait::Same, &["int", "double"]), Some(false));
        assert_eq!(decided(Trait::Same, &["int", "int*"]), Some(false));
        // `size_type` is very often `unsigned long long`, so different spellings are not different types.
        assert_eq!(decided(Trait::Same, &["int", "size_type"]), None);
        assert_eq!(decided(Trait::Same, &["_Ty", "_Uty"]), None);
    }

    #[test]
    fn a_question_about_a_class_body_is_left_open() {
        assert_eq!(decided(Trait::Unknown, &["_Ty", "_Uty"]), None);
        assert_eq!(decided(Trait::Unseen, &["_Ty"]), None);
        // And a trait written with the wrong number of arguments is not guessed at either.
        assert_eq!(decided(Trait::Pointer, &["int", "double"]), None);
        assert_eq!(decided(Trait::Same, &["int"]), None);
    }

    #[test]
    fn the_names_the_headers_write_are_the_names_this_knows() {
        assert_eq!(named("__is_constructible").map(|builtin| builtin.kind), Some(Kind::Trait(Trait::Unknown)));
        assert_eq!(named("__is_pointer").map(|builtin| builtin.kind), Some(Kind::Trait(Trait::Pointer)));
        assert!(named("printf").is_none());
        // **The library's own names are not builtins**, and this is the assertion that keeps a spelling like
        // `std::is_same<_Ty, _Uty>` — a class in a type position — from being read as a `bool`.
        assert!(named("is_same").is_none());
        assert!(named("is_pointer").is_none());
        assert!(named("__is_same").is_some());
    }
}



