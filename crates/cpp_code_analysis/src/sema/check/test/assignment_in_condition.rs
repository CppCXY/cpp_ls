//! **`assignment-in-condition`** — `=` where the reader almost certainly meant `==`.
//!
//! The shape is narrow on purpose: the **whole** condition of an `if`/`while` has to be the assignment, with `=` as the
//! condition's own operator. A comparison, a compound condition (`&&`, a comma, a string literal holding an `=`), an
//! assignment written as a call's argument, and the parenthesised idiom `if ((op = q.value()) != 0)` are all silent,
//! each for a reason a test below states.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **`if (x = 1)` assigns, and the condition is then `1`** — the rule's whole reason to exist. The `while` form is the
/// same question about the same node.
#[test]
fn an_assignment_as_the_whole_condition_is_reported() {
    let if_stat = codes_of("void f() {\n    int x = 0;\n    if (x = 1) { return; }\n}\n");
    assert_reported(
        &if_stat,
        DiagnosticCode::AssignmentInCondition,
        "`x = 1` is the condition",
    );

    let while_stat =
        codes_of("int read();\nvoid f() {\n    int line = 0;\n    while (line = read()) { }\n}\n");
    assert_reported(
        &while_stat,
        DiagnosticCode::AssignmentInCondition,
        "`line = read()` is the condition",
    );
}

/// **A comparison inside the assignment's right-hand side does not hide it**: the condition's own operator is still
/// `=`, and that is the token the rule reads.
#[test]
fn an_assignment_whose_right_side_compares_is_still_reported() {
    let codes = codes_of(
        "void f() {\n    int x = 0;\n    int a = 1;\n    int b = 2;\n    if (x = a < b) { return; }\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AssignmentInCondition,
        "the operator under the condition is `=`",
    );
}

/// **A comparison is not an assignment** — including `>=`, whose `=` the lexer splits into its own token and which the
/// rule must not read as the condition's operator.
#[test]
fn a_comparison_is_silent() {
    let equal = codes_of("void f() {\n    int x = 0;\n    if (x == 1) { return; }\n}\n");
    assert_silent_about(
        &equal,
        DiagnosticCode::AssignmentInCondition,
        "`==` is a comparison",
    );

    let at_least = codes_of("void f(int a, int b) {\n    if (a >= b) { return; }\n}\n");
    assert_silent_about(
        &at_least,
        DiagnosticCode::AssignmentInCondition,
        "the `=` of `>=` is not the condition's operator",
    );
}

/// **A compound condition is refused before any `=` is looked for** — the condition's last operand is what is tested,
/// and a string literal is one token, so an `=` inside it cannot be seen at all.
#[test]
fn a_compound_condition_is_silent() {
    let and_chain = codes_of(
        "void f() {\n    int x = 0;\n    bool ok = true;\n    if (x = 1 && ok) { return; }\n}\n",
    );
    assert_silent_about(
        &and_chain,
        DiagnosticCode::AssignmentInCondition,
        "the condition is a `&&`, not an assignment",
    );

    let comma = codes_of(
        "void f() {\n    int x = 0;\n    int y = 1;\n    if (x = y, x > 0) { return; }\n}\n",
    );
    assert_silent_about(
        &comma,
        DiagnosticCode::AssignmentInCondition,
        "the comma expression's last operand is what is tested",
    );

    let inside_a_literal =
        codes_of("void f() {\n    const char* s = 0;\n    if (s == \"a=b\") { return; }\n}\n");
    assert_silent_about(
        &inside_a_literal,
        DiagnosticCode::AssignmentInCondition,
        "the `=` is inside a string literal",
    );
}

/// **An assignment that is not the condition**: an argument to a call, and the parenthesised assignment whose result is
/// compared — the idiom a compiler warns about and a reader wrote deliberately.
#[test]
fn an_assignment_inside_an_expression_is_silent() {
    let argument =
        codes_of("void g(int v);\nvoid f() {\n    int x = 0;\n    if (g(x = 1)) { return; }\n}\n");
    assert_silent_about(
        &argument,
        DiagnosticCode::AssignmentInCondition,
        "the assignment is an argument, not the condition",
    );

    let idiom = codes_of(
        "struct Q { int value() const; };\nvoid f(Q q) {\n    int op = 0;\n    if ((op = q.value()) != 0) { return; }\n}\n",
    );
    assert_silent_about(
        &idiom,
        DiagnosticCode::AssignmentInCondition,
        "the assignment is parenthesised and its result is compared",
    );
}
