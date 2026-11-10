//! **`simplify-boolean-expr`** — an `if` condition that compares against `true` or `false` is already the value.
//!
//! A hint rather than a law, and one that reads the condition's **text**: `== true`, `== false` and their mirrored
//! spellings are what it looks for, and only in an `if`. A comparison against another value, a negation, a `while`
//! condition and a comparison outside a condition are all left alone, and each is silent below for its own reason.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The rule's own spellings**: `x == true`, `x == false`, and the mirrored `true == x`.
#[test]
fn a_condition_compared_against_a_literal_is_reported() {
    let against_true = codes_of("void f(bool ready) {\n    if (ready == true) { return; }\n}\n");
    assert_reported(
        &against_true,
        DiagnosticCode::SimplifyBooleanExpr,
        "`ready == true` is `ready`",
    );

    let against_false = codes_of("void f(bool ready) {\n    if (ready == false) { return; }\n}\n");
    assert_reported(
        &against_false,
        DiagnosticCode::SimplifyBooleanExpr,
        "`ready == false` is `!ready`",
    );

    let mirrored = codes_of("void f(bool ready) {\n    if (true == ready) { return; }\n}\n");
    assert_reported(
        &mirrored,
        DiagnosticCode::SimplifyBooleanExpr,
        "the literal may stand on either side of the comparison",
    );
}

/// **A condition that is already the value is the answer the rule asks for**, in its plain and negated forms.
#[test]
fn a_condition_that_is_already_the_value_is_quiet() {
    let plain = codes_of("void f(bool ready) {\n    if (ready) { return; }\n}\n");
    assert_silent_about(
        &plain,
        DiagnosticCode::SimplifyBooleanExpr,
        "`if (ready)` is the sentence the rule would rewrite `ready == true` to",
    );

    let negated = codes_of("void f(bool ready) {\n    if (!ready) { return; }\n}\n");
    assert_silent_about(
        &negated,
        DiagnosticCode::SimplifyBooleanExpr,
        "`!ready` is already the negation the rule would suggest",
    );
}

/// **A comparison against another value is not a comparison against the literal**, and `!=` is not one of the four
/// spellings the rule reads.
#[test]
fn a_comparison_against_another_value_is_quiet() {
    let against_a_number = codes_of("void f(int count) {\n    if (count == 1) { return; }\n}\n");
    assert_silent_about(
        &against_a_number,
        DiagnosticCode::SimplifyBooleanExpr,
        "`1` is a number, not the literal `true` this rule is about",
    );

    let two_values =
        codes_of("void f(int left, int right) {\n    if (left == right) { return; }\n}\n");
    assert_silent_about(
        &two_values,
        DiagnosticCode::SimplifyBooleanExpr,
        "two values compared with each other are already the expression",
    );

    let an_inequality = codes_of("void f(bool ready) {\n    if (ready != true) { return; }\n}\n");
    assert_silent_about(
        &an_inequality,
        DiagnosticCode::SimplifyBooleanExpr,
        "`!=` is not one of the four spellings the rule looks for",
    );
}

/// **Only an `if`'s condition is read** — the walk is over `if` statements, so a `while` head and a comparison in an
/// initialiser are not this rule's text.
#[test]
fn only_an_if_condition_is_read() {
    let in_a_loop = codes_of("void f(bool ready) {\n    while (ready == false) { break; }\n}\n");
    assert_silent_about(
        &in_a_loop,
        DiagnosticCode::SimplifyBooleanExpr,
        "the rule walks `if` statements, and a `while` head is a different statement",
    );

    let outside = codes_of("void f(bool ready) {\n    bool same = ready == true;\n}\n");
    assert_silent_about(
        &outside,
        DiagnosticCode::SimplifyBooleanExpr,
        "a comparison in an initialiser is not a condition",
    );
}
