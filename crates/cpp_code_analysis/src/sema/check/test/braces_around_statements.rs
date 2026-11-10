//! **`braces-around-statements`** — a body that is one statement should be a block.
//!
//! The walk is over **`if` statements**, and the body it looks at is the **then-branch**: when that branch's text does
//! not begin with `{`, it is one statement. That is the shape of the dangling-`else` bug — `if (a) if (b) f(); else
//! g();` binds the `else` to the inner `if` — which is why the rule is a diagnostic rather than a matter of taste.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A one-statement then-branch is reported**, an expression statement and a bare `return` alike.
#[test]
fn a_one_statement_then_branch_is_reported() {
    let call = codes_of("void start();\nvoid f(bool ready) {\n    if (ready) start();\n}\n");
    assert_reported(
        &call,
        DiagnosticCode::BracesAroundStatements,
        "`start();` is one statement",
    );

    let returning = codes_of("void f(bool ready) {\n    if (ready) return;\n}\n");
    assert_reported(
        &returning,
        DiagnosticCode::BracesAroundStatements,
        "`return;` is one statement",
    );
}

/// **The dangling `else` is two one-statement bodies**: the outer `if`'s branch *is* the inner `if`, and the inner
/// one's branch is the call.
#[test]
fn a_dangling_else_is_two_one_statement_bodies() {
    let codes = codes_of(
        "void act();\nvoid f(bool a, bool b) {\n    if (a) if (b) act(); else act();\n}\n",
    );
    let reported = codes
        .iter()
        .filter(|code| **code == DiagnosticCode::BracesAroundStatements)
        .count();
    assert_eq!(
        reported, 2,
        "the outer branch is an `if` and the inner one a call: {codes:?}"
    );
}

/// **A braced body is what the rule asks for**, and an empty block is a block too.
#[test]
fn a_braced_body_is_silent() {
    let block = codes_of("void start();\nvoid f(bool ready) {\n    if (ready) { start(); }\n}\n");
    assert_silent_about(
        &block,
        DiagnosticCode::BracesAroundStatements,
        "the branch begins with `{`",
    );

    let empty = codes_of("void f(bool ready) {\n    if (ready) { }\n}\n");
    assert_silent_about(
        &empty,
        DiagnosticCode::BracesAroundStatements,
        "an empty `{ }` is still a block",
    );
}

/// **A `while` body is not this walk.** The check asks about `if` statements — the shape the dangling `else` lives in
/// — so a one-line loop body is not one of its findings.
#[test]
fn a_one_statement_while_body_is_silent() {
    let codes = codes_of("void start();\nvoid f(bool ready) {\n    while (ready) start();\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::BracesAroundStatements,
        "the walk covers `if` statements",
    );
}

/// **The then-branch is what is examined**: `if (x) { a(); } else b();` writes its `else` body as one statement, which
/// is not the question this check asks.
#[test]
fn a_braced_then_branch_with_an_unbraced_else_is_silent() {
    let codes =
        codes_of("void a();\nvoid b();\nvoid f(bool x) {\n    if (x) { a(); } else b();\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::BracesAroundStatements,
        "the then-branch is a block, and the `else` body is not read",
    );
}
