//! **`final-misplaced`** — `final` on a function that is neither virtual nor overriding.
//!
//! The legal shapes are the silences below: `virtual … final` (the fact records `virtual`), and `final` on a member
//! that overrides a base's virtual. The illegal ones are a `final` with no base to override, a base that does not name
//! the function, and a free function.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **`final` with no base**: nothing to override, and the function is not virtual.
#[test]
fn final_in_a_class_with_no_base_is_reported() {
    let codes = codes_of("struct Widget {\n    void size() final;\n};\n");
    assert_reported(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`final` needs a virtual function",
    );
}

/// **`final` on a name no base declares**: still not virtual.
#[test]
fn final_on_a_name_no_base_declares_is_reported() {
    let codes = codes_of(
        "struct Base { virtual void run(); };\nstruct Derived : Base { void walk() final; };\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`Base` declares `run`, not `walk`",
    );
}

/// **`final` on a free function** — a free function is never virtual.
#[test]
fn final_on_a_free_function_is_reported() {
    let codes = codes_of("void run() final;\n");
    assert_reported(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "a free function cannot be virtual",
    );
}

/// **`virtual … final` is the legal spelling** and must stay silent.
#[test]
fn virtual_final_is_silent() {
    let codes = codes_of("struct Widget {\n    virtual void draw() final;\n};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`virtual` and `final` together are what the standard allows",
    );
}

/// **`final` on an override is legal**, even without repeating `virtual`.
#[test]
fn final_on_an_override_is_silent() {
    let codes = codes_of(
        "struct Base { virtual void run(); };\nstruct Derived : Base { void run() final; };\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`run` overrides `Base::run`, so it is virtual",
    );
}

/// **A base whose header was never read** is silence.
#[test]
fn an_unreadable_base_is_not_judged() {
    let codes = codes_of("struct Derived : NotHere { void walk() final; };\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`NotHere` was not read, so `walk` may override a virtual of it",
    );
}
/// **A destructor marked `final`** overrides `~A` and is legal; the names do not match, so it is not judged by name.
#[test]
fn a_final_destructor_is_silent() {
    let codes = codes_of("struct A { virtual ~A(); };\nstruct B : A { ~B() final; };\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::FinalMisplaced,
        "`~B` overrides `~A`",
    );
}