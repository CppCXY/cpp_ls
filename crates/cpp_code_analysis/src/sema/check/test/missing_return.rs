//! **`missing-return`** — a function whose written return type is not `void` and whose body can reach its closing
//! brace.
//!
//! The rule is a control-flow question, not a search for the word `return`: `int f(bool x) { if (x) return 1; }` has
//! one and still falls off its end, while `int g() { throw 1; }` has none and cannot. A shape is silent only when the
//! walk can **prove** no path reaches the end, and the negative half of this file is those proofs.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The rule's whole reason to exist**: a body that returns nothing, and a body with one path that does.
#[test]
fn a_body_that_can_reach_its_closing_brace_is_reported() {
    let empty = codes_of("int count() { }\n");
    assert_reported(
        &empty,
        DiagnosticCode::MissingReturn,
        "an empty body returns nothing and the type is not `void`",
    );

    let one_path = codes_of("int only_one(int x) { if (x) return 1; }\n");
    assert_reported(
        &one_path,
        DiagnosticCode::MissingReturn,
        "a `return` in one branch is not a `return` on every path",
    );

    let returns = codes_of("int count() { return 1; }\n");
    assert_silent_about(
        &returns,
        DiagnosticCode::MissingReturn,
        "the ordinary body that returns",
    );
}

/// **Nothing is expected from `void`, from a deduced type, or from a declaration** — a promise rather than a body.
#[test]
fn a_void_or_a_deduced_type_promises_nothing() {
    let stop = codes_of("void stop() { }\n");
    assert_silent_about(
        &stop,
        DiagnosticCode::MissingReturn,
        "`void` is the one type that expects no return",
    );

    let deduced = codes_of("auto deduced(int x) { if (x) return 1; }\n");
    assert_silent_about(
        &deduced,
        DiagnosticCode::MissingReturn,
        "a deduced return type is a deduction question, which this rule does not read",
    );

    let promised = codes_of("int promised();\n");
    assert_silent_about(
        &promised,
        DiagnosticCode::MissingReturn,
        "`int promised();` declares no body, and a declaration is a promise",
    );
}

/// **The other direction of the control-flow question**: a body no path leaves normally needs no `return`.
#[test]
fn a_body_no_path_leaves_normally_is_silent() {
    let both_branches = codes_of("int both(int x) { if (x) return 1; else return 2; }\n");
    assert_silent_about(
        &both_branches,
        DiagnosticCode::MissingReturn,
        "both branches return, so neither path reaches the brace",
    );

    let throws = codes_of("int boom() { throw 1; }\n");
    assert_silent_about(
        &throws,
        DiagnosticCode::MissingReturn,
        "a body that always throws cannot fall off its end",
    );

    let endless = codes_of("int forever() { while (true) { } }\n");
    assert_silent_about(
        &endless,
        DiagnosticCode::MissingReturn,
        "an endless loop with no `break` never reaches the brace",
    );

    let for_ever = codes_of("int for_ever() { for (;;) { } }\n");
    assert_silent_about(
        &for_ever,
        DiagnosticCode::MissingReturn,
        "`for (;;)` writes no condition to leave the loop on",
    );
}

/// **A constructor and a destructor write no return type**, whatever decorates them — and the ordinary member that
/// does write one is still reported, so the test cannot pass with the rule switched off.
#[test]
fn a_constructor_and_a_destructor_promise_no_return() {
    let source = "\
struct S {
    explicit S(int x) : x_(x) {}
    ~S() {}
    int x_;
};
";
    assert_silent_about(
        &codes_of(source),
        DiagnosticCode::MissingReturn,
        "a constructor and a destructor promise no return, however they are written",
    );

    let a_member = codes_of("struct T { int broken() { } };\n");
    assert_reported(
        &a_member,
        DiagnosticCode::MissingReturn,
        "a member function that writes `int` and returns nothing is still an ordinary function",
    );
}
