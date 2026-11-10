//! **`use-nullptr`** — `NULL` where `nullptr` is meant: the macro is `0`, which is an `int`.
//!
//! The spelling is the whole evidence: an expression whose text is exactly `NULL`. Resolving the macro would answer
//! `0` and lose the fact the rule is about, which is that the reader wrote a constant with the wrong type — so a
//! comment, a string, `nullptr` and a plain `0` are all silence.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The bug the rule prevents**: with `void f(int)` beside `void f(int*)`, `f(NULL)` calls the `int` overload.
#[test]
fn null_in_a_call_argument_is_reported() {
    let codes = codes_of("void f(int*);\nvoid g() {\n    f(NULL);\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UseNullptr,
        "`NULL` is `0` in every standard header",
    );
}

/// **An initialiser is the same spelling and the same question.**
#[test]
fn null_in_an_initialiser_is_reported() {
    let codes = codes_of("void g() {\n    int *p = NULL;\n    (void)p;\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UseNullptr,
        "the pointer is initialised with the macro",
    );
}

/// **`nullptr` and `0` are the two spellings that are not the macro**: one has the pointer type, and the other is
/// what the macro expands to — a constant whose intent the text does not state.
#[test]
fn nullptr_and_zero_are_silent() {
    let nullptr = codes_of("void g() {\n    int *p = nullptr;\n    (void)p;\n}\n");
    assert_silent_about(
        &nullptr,
        DiagnosticCode::UseNullptr,
        "`nullptr` has the pointer type",
    );

    let zero = codes_of("void g() {\n    int *p = 0;\n    (void)p;\n}\n");
    assert_silent_about(
        &zero,
        DiagnosticCode::UseNullptr,
        "the rule is about the spelling `NULL`",
    );
}

/// **Prose and data are not spellings**: a comment and a string literal both contain `NULL`, and neither is an
/// expression.
#[test]
fn prose_and_data_are_not_expressions() {
    let commented = codes_of("// NULL is what we used to write\nint f() { return 0; }\n");
    assert_silent_about(
        &commented,
        DiagnosticCode::UseNullptr,
        "a comment is not a use",
    );

    let quoted = codes_of("void f();\nvoid g() { f(\"NULL\"); }\n");
    assert_silent_about(
        &quoted,
        DiagnosticCode::UseNullptr,
        "a string literal is not a use",
    );
}
