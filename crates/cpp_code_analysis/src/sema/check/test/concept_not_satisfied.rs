//! **`concept-not-satisfied`** — an argument whose type does not satisfy the concept its parameter is constrained by.
//!
//! Only the concepts whose meaning is a property of the type are decided — `std::integral`, `std::floating_point`,
//! `std::derived_from`, and the "object" family, which answers about references alone. A user-written concept, a
//! constraint this layer cannot read and a name with two declarations are all silent: a finding is a claim, and a claim
//! about a concept nobody evaluated would be a guess.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **`const char*` is not `std::integral`**, and the call that does satisfy the constraint stays quiet.
#[test]
fn an_argument_that_does_not_satisfy_integral_is_reported() {
    let codes = codes_of(
        "template <std::integral T>\nT twice(T value) { return value + value; }\n\n\
         int f() {\n    twice(2);\n    twice(\"text\");\n    return 0;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::ConceptNotSatisfied,
        "`twice(\"text\")` passes a `const char*`",
    );
}

/// **An integer is not a `std::floating_point`**, and a `float` is — the concept is answered from the type's own shape.
#[test]
fn an_integer_is_not_a_floating_point() {
    let wrong = codes_of(
        "template <std::floating_point T>\nT half(T value) { return value; }\n\n\
         int f() {\n    half(1);\n    return 0;\n}\n",
    );
    assert_reported(
        &wrong,
        DiagnosticCode::ConceptNotSatisfied,
        "`int` is not `std::floating_point`",
    );

    let right = codes_of(
        "template <std::floating_point T>\nT half(T value) { return value; }\n\n\
         int f() {\n    float x = 1.0f;\n    half(x);\n    return 0;\n}\n",
    );
    assert_silent_about(
        &right,
        DiagnosticCode::ConceptNotSatisfied,
        "`float` is a floating-point type",
    );
}

/// **`std::derived_from` is answered from the base clauses**: a class that names the base is silent, and one that names
/// nothing — or a builtin, which has no bases at all — is reported.
#[test]
fn a_class_that_does_not_derive_from_the_base_is_reported() {
    let codes = codes_of(
        "struct Base { virtual ~Base(); };\nstruct Derived : public Base { };\nstruct Plain { };\n\n\
         template <std::derived_from<Base> T>\nvoid handle(T& value);\n\n\
         void f(Derived d, Plain p, int n) {\n    handle(d);\n    handle(p);\n    handle(n);\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::ConceptNotSatisfied,
        "`Plain` and `int` derive from nothing, while `Derived` derives from `Base`",
    );
}

/// **A user-written concept is silence** — its meaning is a `requires`-expression this layer does not evaluate.
#[test]
fn a_user_written_concept_is_silent() {
    let codes = codes_of(
        "template <class T>\nconcept Small = sizeof(T) <= 4;\n\n\
         template <Small T>\nT keep(T value) { return value; }\n\n\
         int f() {\n    keep(1.0);\n    return 0;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ConceptNotSatisfied,
        "`Small` is not a concept this layer evaluates",
    );
}

/// **An overload set is not judged**: which overload's constraint applies is overload resolution, so two declarations
/// of the name keep the check quiet.
#[test]
fn an_overload_set_is_silent() {
    let codes = codes_of(
        "template <std::integral T>\nvoid pick(T value);\ntemplate <std::integral T>\nvoid pick(T value, int extra);\n\n\
         void f() {\n    pick(\"text\");\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ConceptNotSatisfied,
        "two declarations are an overload set",
    );
}

/// **An "object" concept over a value type is silence**: a class's answer needs its constructors, which this layer does
/// not read, so only a reference is definitely not an object type.
#[test]
fn an_object_concept_over_a_value_type_is_silent() {
    let codes = codes_of(
        "template <std::copyable T>\nT copy_of(T value) { return value; }\n\n\
         int f(int n) {\n    copy_of(n);\n    return 0;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ConceptNotSatisfied,
        "a value type is not answered about",
    );
}
