//! **`cstyle-cast`** — a cast written in the C style, `(Widget*)p`, where C++ has four named casts.
//!
//! The question is structural: a cast that **opens with its `(`** and names a type. A named cast opens with its
//! keyword, a functional cast is a call, a parenthesised expression is not a cast at all, and `(void)x` is the
//! language's way of discarding a value rather than converting it.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A C-style cast to a builtin** is the shape the rule exists for: the parentheses name a type, so this is a cast.
#[test]
fn a_c_style_cast_to_a_builtin_is_reported() {
    let codes = codes_of("void f(double n) {\n    int i = (int)n;\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::CStyleCast,
        "`(int)n` names a type in parentheses",
    );
}

/// **A pointer cast is the same shape**, and a cast that drops `const` is the defect the rule is borrowed for.
#[test]
fn a_c_style_cast_to_a_pointer_is_reported() {
    let made = codes_of(
        "struct Widget { int size; };\nvoid f(void* p) {\n    Widget* w = (Widget*)p;\n}\n",
    );
    assert_reported(&made, DiagnosticCode::CStyleCast, "`(Widget*)p` is a cast");

    let without_const = codes_of("void f(const int* p) {\n    int* q = (int*)p;\n}\n");
    assert_reported(
        &without_const,
        DiagnosticCode::CStyleCast,
        "a C-style cast can drop `const` without saying so",
    );
}

/// **`(void)x` is not a conversion** — it is how the language discards a value, and no named cast spells it.
#[test]
fn a_cast_to_void_is_silent() {
    let codes = codes_of("void f(int n) {\n    (void)n;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::CStyleCast,
        "a cast to `void` converts nothing",
    );
}

/// **The named casts and a functional cast are different shapes**: one opens with its keyword, the other is a call.
#[test]
fn the_named_casts_are_not_c_style() {
    let named = codes_of("void f(double n) {\n    int i = static_cast<int>(n);\n}\n");
    assert_silent_about(
        &named,
        DiagnosticCode::CStyleCast,
        "`static_cast` says which conversion is meant",
    );

    let functional = codes_of("void f(double n) {\n    int i = int(n);\n}\n");
    assert_silent_about(
        &functional,
        DiagnosticCode::CStyleCast,
        "`int(n)` is a functional cast, parsed as a call",
    );
}

/// **Parentheses are not a cast**: `(g())` names no type, and `(a + b) * c` is arithmetic.
#[test]
fn parentheses_that_are_not_a_cast_are_silent() {
    let parenthesised = codes_of("int g();\nvoid f() {\n    int i = (g());\n}\n");
    assert_silent_about(
        &parenthesised,
        DiagnosticCode::CStyleCast,
        "`(g())` is a parenthesised call",
    );

    let arithmetic = codes_of("int g(int a, int b, int c) { return (a + b) * c; }\n");
    assert_silent_about(
        &arithmetic,
        DiagnosticCode::CStyleCast,
        "`(a + b)` is an expression, not a type",
    );
}
