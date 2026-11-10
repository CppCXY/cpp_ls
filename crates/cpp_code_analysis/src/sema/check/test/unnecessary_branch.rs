//! **`unnecessary-branch`** — a branch the program cannot take, or one whose condition is already decided.
//!
//! The rule reads a **literal written where the condition goes** and nothing else: `if (true)` and `if (false)` are
//! facts about the text, while `if (DEBUG)`, `if (1 == 1)` and `while (true)` are questions this check deliberately
//! does not answer. Folding a macro or an expression is a constant folder's work, and doing half of it would be wrong
//! in the half it did — which is the line every negative below is drawn on.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **Both directions of the literal**: `false` is a body that never runs, and `true` a decision already made.
#[test]
fn a_literal_condition_is_reported() {
    let never = codes_of("int main() { if (false) { return 1; } return 0; }\n");
    assert_reported(
        &never,
        DiagnosticCode::UnnecessaryBranch,
        "`if (false)` cannot run",
    );

    let always = codes_of("void f() { if (true) { } }\n");
    assert_reported(
        &always,
        DiagnosticCode::UnnecessaryBranch,
        "`if (true)` decides nothing",
    );
}

/// **Parentheses around the literal do not hide it** — the condition is read by token kind, and parentheses are
/// dropped before the one word is counted.
#[test]
fn parentheses_do_not_hide_the_literal() {
    let codes = codes_of("void f() { if ((false)) { } }\n");
    assert_reported(
        &codes,
        DiagnosticCode::UnnecessaryBranch,
        "`(false)` is still a bare literal",
    );
}

/// **A name is a question, not a fact about the text** — the ruling line of the check.
#[test]
fn a_name_is_not_a_literal() {
    let codes = codes_of("int main() { if (DEBUG) { return 1; } return 0; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::UnnecessaryBranch,
        "`DEBUG` is a name, and folding it is another layer's question",
    );
}

/// **Nothing is folded**: an arithmetic comparison is a constant expression this check does not evaluate, and a
/// `while` head is a different statement from the `if` it walks.
#[test]
fn nothing_is_folded() {
    let comparison = codes_of("void f() { if (1 == 1) { } }\n");
    assert_silent_about(
        &comparison,
        DiagnosticCode::UnnecessaryBranch,
        "`1 == 1` needs a constant folder",
    );

    let looped = codes_of("void f() { while (true) { } }\n");
    assert_silent_about(
        &looped,
        DiagnosticCode::UnnecessaryBranch,
        "the rule walks `if` statements, and a `while` head is a different statement",
    );
}

/// **A comment and a string are not conditions** — the tree has no `if` there to judge.
#[test]
fn prose_and_data_are_not_conditions() {
    let commented = codes_of("// if (false) { return; }\nvoid f() { }\n");
    assert_silent_about(
        &commented,
        DiagnosticCode::UnnecessaryBranch,
        "a comment is not a statement",
    );

    let quoted = codes_of("void g(const char*);\nvoid f() { g(\"if (false) { }\"); }\n");
    assert_silent_about(
        &quoted,
        DiagnosticCode::UnnecessaryBranch,
        "a string literal is not a condition",
    );
}
