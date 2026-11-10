//! **`return-type-mismatch`** — a `return` whose expression does not convert to the function's written return type.
//!
//! Two shapes are reported: an expression that does not convert, and a bare `return;` from a function that is not
//! `void`. Everything else is either the conversion question's `Unknown` or a shape the rule does not judge at all — a
//! class that may have a conversion constructor, a value returned from a `void` function, an `auto` return type — and
//! a check that reported on `Unknown` would be reporting about code it cannot see.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The two reported shapes**: a value that does not convert, and no value where one is promised.
#[test]
fn the_two_reported_shapes_are_a_bad_value_and_a_bare_return() {
    let a_string_for_an_int = codes_of("int count() { return \"three\"; }\n");
    assert_reported(
        &a_string_for_an_int,
        DiagnosticCode::ReturnTypeMismatch,
        "a `const char*` does not convert to the `int` this function returns",
    );

    let a_bare_return = codes_of("int value() { return; }\n");
    assert_reported(
        &a_bare_return,
        DiagnosticCode::ReturnTypeMismatch,
        "`return;` gives nothing to a function declared to return `int`",
    );
}

/// **A `void` function is legal in both spellings**: `return;` expects nothing, and `return f();` where `f` returns
/// `void` is the ordinary early exit.
#[test]
fn a_void_function_returns_nothing() {
    let bare = codes_of("void stop() { return; }\n");
    assert_silent_about(
        &bare,
        DiagnosticCode::ReturnTypeMismatch,
        "`void` expects no value, and `return;` gives none",
    );

    let a_call = codes_of("void helper() {}\nvoid forward() { return helper(); }\n");
    assert_silent_about(
        &a_call,
        DiagnosticCode::ReturnTypeMismatch,
        "a `void` expression returned from a `void` function is legal C++",
    );
}

/// **A deduced type is a deduction question, and an arithmetic conversion is still a conversion.**
#[test]
fn a_deduced_type_or_a_converting_value_is_quiet() {
    let deduced = codes_of("auto make() { return 1; }\n");
    assert_silent_about(
        &deduced,
        DiagnosticCode::ReturnTypeMismatch,
        "`auto` means the return type is deduced, not compared",
    );

    let same = codes_of("int same() { return 1; }\n");
    assert_silent_about(
        &same,
        DiagnosticCode::ReturnTypeMismatch,
        "an `int` where an `int` is declared is the same type",
    );

    let narrows = codes_of("int narrows() { return 1.5; }\n");
    assert_silent_about(
        &narrows,
        DiagnosticCode::ReturnTypeMismatch,
        "`double` to `int` is an arithmetic conversion, which the type relation answers `Yes` to",
    );
}

/// **The conversions this layer refuses to decide are silent**: a class value against an integer could be a
/// conversion operator the model never reads, and reporting it would be a finding about unseen code.
#[test]
fn a_class_value_is_not_judged_against_an_integer() {
    let source = "\
struct Widget { int size; };
Widget make();
int f() {
    Widget w = make();
    return w;
}
";
    assert_silent_about(
        &codes_of(source),
        DiagnosticCode::ReturnTypeMismatch,
        "a `Widget` may have a conversion operator, so `Widget` to `int` is `Unknown`",
    );

    let a_bool = codes_of("int count() { bool ready = true; return ready; }\n");
    assert_silent_about(
        &a_bool,
        DiagnosticCode::ReturnTypeMismatch,
        "`bool` to `int` is an arithmetic conversion, not a mismatch",
    );
}
