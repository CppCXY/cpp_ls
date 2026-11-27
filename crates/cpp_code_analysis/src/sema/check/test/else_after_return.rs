//! **`else-after-return`** — an `else` whose `if` branch returns does not need to be an `else`.
//!
//! The rule is a hint about nesting, and its silence is where the care is: an `if` with no `else` is the early return
//! the rule is *for*, an `else` whose then-branch does not return is a branch that is needed, and a compile-time `if`
//! is a table rather than nesting to flatten. The return is asked of the **tree**, so a comment, a string or a
//! variable that merely spells `return` is not one.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The shape the rule exists for**: a then-branch that returns, and an `else` after it.
#[test]
fn an_else_after_a_returning_branch_is_reported() {
    let braced =
        codes_of("void f(bool ready) {\n    if (ready) { return; } else { g(); }\n}\nvoid g();\n");
    assert_reported(
        &braced,
        DiagnosticCode::ElseAfterReturn,
        "the then-branch returns, so the `else` nests what does not need nesting",
    );

    let chain = codes_of(
        "int f(int a, int b) {\n\
             if (a) { return 1; } else if (b) { return 2; }\n\
             return 0;\n\
         }\n",
    );
    assert_reported(
        &chain,
        DiagnosticCode::ElseAfterReturn,
        "the outer `if` returns, and its `else` is the next link of the chain",
    );
}

/// **Only a branch that leaves the function on every path makes the `else` redundant.** A `return` nested inside
/// the branch is not one: this `if (a) { if (b) return; g(); }` falls through to `h()` when `b` is false, so
/// dropping the `else` would change the program — and the `g()` after the return is unreachable only in the path
/// that already left.
///
/// The first version of the rule asked whether the branch **contained** a return, and the module recorded the
/// stricter reading as "a refinement that can land later". Measured on an ordinary C file — a `switch` arm writing
/// `if (at < 0x20) { if (!emit(...)) return 0; } else if (!push_char(...)) return 0;` — that finding was the only
/// one in the whole project, on correct code, and acting on it is a syntax error.
#[test]
fn a_return_on_one_path_is_not_a_branch_that_returns() {
    let codes = codes_of(
        "void g();\n\
         void h();\n\
         void f(int a, int b) {\n\
             if (a) {\n\
                 if (b) { return; }\n\
                 g();\n\
             } else {\n\
                 h();\n\
             }\n\
         }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ElseAfterReturn,
        "the then-branch falls through to `h()` when `b` is false",
    );

    // …and a branch that really does end in the return still is one, with work before it.
    let ends_in_a_return = codes_of(
        "void g();\n\
         void f(int a) {\n\
             if (a) { g(); return; } else { g(); }\n\
         }\n",
    );
    assert_reported(
        &ends_in_a_return,
        DiagnosticCode::ElseAfterReturn,
        "the last statement of the then-branch is the return",
    );
}

/// **An `if` with no `else` is the early return the rule recommends**, so there is nothing to report.
#[test]
fn an_if_without_an_else_is_not_reported() {
    let codes = codes_of("void g();\nvoid f(bool ready) { if (ready) { return; } g(); }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::ElseAfterReturn,
        "there is no `else` to drop",
    );
}

/// **An `else` whose then-branch does not return is a branch that is needed** — and a return in the `else` itself is
/// not a return *before* the `else`.
#[test]
fn an_else_whose_then_branch_does_not_return_is_not_reported() {
    let neither =
        codes_of("int f(int x) { int r = 0; if (x) { r = 1; } else { r = 2; } return r; }\n");
    assert_silent_about(
        &neither,
        DiagnosticCode::ElseAfterReturn,
        "neither branch returns",
    );

    let only_the_else =
        codes_of("int f(int x) { int r = 0; if (x) { r = 1; } else { return 2; } return r; }\n");
    assert_silent_about(
        &only_the_else,
        DiagnosticCode::ElseAfterReturn,
        "the return is inside the `else`, not before it",
    );
}

/// **The question is the tree's**: a comment, a string literal and a variable may all spell `return` without being
/// a return statement.
#[test]
fn a_word_that_spells_return_is_not_a_return() {
    let codes = codes_of(
        "void g(const char*);\n\
         void f(bool ready) {\n\
             int returns = 0;\n\
             if (ready) { /* return early */ returns = 1; g(\"return\"); } else { returns = 2; }\n\
             (void)returns;\n\
         }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ElseAfterReturn,
        "nothing in the then-branch is a `return` statement",
    );
}

/// **A compile-time `if` keeps its `else`**: `if constexpr (…) A else B` is one construct whose branch the compiler
/// chooses, and its ladder is read as a table rather than as nesting to flatten.
#[test]
fn a_compile_time_if_keeps_its_else() {
    let codes = codes_of(
        "template <class T>\n\
         int f(T value) {\n\
             if constexpr (sizeof(T) == 4) { return 1; } else { return 2; }\n\
         }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ElseAfterReturn,
        "the `else` of an `if constexpr` is part of the construct",
    );
}
