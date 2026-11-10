//! **`implicit-bool-conversion`** — an arithmetic condition converts to `bool`, and the conversion is what the rule
//! reports.
//!
//! This is the rule that needs the analysis: the spelling of `if (ready)` does not say whether `ready` is a `bool` or
//! an `int`, and only the type does. The silence is the larger half — a `bool` is the type the conversion would
//! produce, a pointer is the nullable-pointer idiom, a class with `operator bool` is the C++ way to say "test me", and
//! a comparison or a `!` is `bool` by its own syntax, without an inference being asked.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain case**: an arithmetic operand asked as a truth value, in an `if` and in a `while`.
#[test]
fn an_arithmetic_condition_is_reported() {
    let if_statement = codes_of("void f(int count) { if (count) { return; } }\n");
    assert_reported(
        &if_statement,
        DiagnosticCode::ImplicitBoolConversion,
        "an `int` in a condition is converted to `bool`",
    );

    let looped = codes_of("void f(int n) { while (n) { --n; } }\n");
    assert_reported(
        &looped,
        DiagnosticCode::ImplicitBoolConversion,
        "`while` asks the same question as `if`",
    );

    let unsigned = codes_of("void f(unsigned count) { if (count) { return; } }\n");
    assert_reported(
        &unsigned,
        DiagnosticCode::ImplicitBoolConversion,
        "every arithmetic type is converted, not only `int`",
    );
}

/// **A `bool` is the type the conversion would produce**, so there is no conversion for the reader to be told about.
#[test]
fn a_bool_condition_is_not_a_conversion() {
    let parameter = codes_of("void f(bool ready) { if (ready) { return; } }\n");
    assert_silent_about(
        &parameter,
        DiagnosticCode::ImplicitBoolConversion,
        "`bool` converts to itself",
    );

    let looped = codes_of("void f(bool ready) { while (ready) { ready = false; } }\n");
    assert_silent_about(
        &looped,
        DiagnosticCode::ImplicitBoolConversion,
        "and `while` reads the same `bool`",
    );
}

/// **`if (ptr)` is the nullable-pointer idiom** — the same implicit conversion, and the one every C and C++ codebase
/// writes; the rule is deliberately silent on it.
#[test]
fn a_pointer_condition_is_not_reported() {
    let parameter = codes_of("void f(int* p) { if (p) { return; } }\n");
    assert_silent_about(
        &parameter,
        DiagnosticCode::ImplicitBoolConversion,
        "a pointer is the idiom the rule leaves alone",
    );

    let member = codes_of("struct S { int* p; };\nvoid f(S* s) { if (s->p) { return; } }\n");
    assert_silent_about(
        &member,
        DiagnosticCode::ImplicitBoolConversion,
        "a pointer reached through a member is still a pointer",
    );
}

/// **A comparison or a logical operator answers itself** — it is `bool` by its own syntax, so no type has to be
/// inferred and no conversion is reported.
#[test]
fn a_comparison_and_a_logical_operator_are_not_conversions() {
    let equal = codes_of("void f(int a, int b) { if (a == b) { return; } }\n");
    assert_silent_about(
        &equal,
        DiagnosticCode::ImplicitBoolConversion,
        "`==` produces a `bool`",
    );

    let ordered = codes_of("void f(int a, int b) { if (a < b) { return; } }\n");
    assert_silent_about(
        &ordered,
        DiagnosticCode::ImplicitBoolConversion,
        "`<` produces a `bool`",
    );

    let conjunction = codes_of("void f(int a, int b) { if (a && b) { return; } }\n");
    assert_silent_about(
        &conjunction,
        DiagnosticCode::ImplicitBoolConversion,
        "`&&` is the predicate operator itself",
    );

    let negated = codes_of("void f(int n) { if (!n) { return; } }\n");
    assert_silent_about(
        &negated,
        DiagnosticCode::ImplicitBoolConversion,
        "`!` already reads its operand as a predicate",
    );
}

/// **A class that says how to be tested is not an arithmetic conversion** — `operator bool` is the C++ spelling of
/// "this is testable", which is another question from the one this rule asks.
#[test]
fn a_class_that_declares_operator_bool_is_not_reported() {
    let codes = codes_of(
        "struct Flag { operator bool() const; };\nvoid f(Flag flag) { if (flag) { return; } }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ImplicitBoolConversion,
        "a class is not an arithmetic type",
    );
}
