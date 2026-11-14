//! **`contract-on-virtual-function` / `contract-on-defaulted-function`** — C++26's two refused contract placements.
//!
//! The grammar reads a contract specifier on any function declarator; the language refuses two of them, and the
//! silences below are the ones that keep the rule honest: a contract on an ordinary function, on a defaulted function
//! that is *not* its own first declaration (the standard allows that), and a virtual function with no contract at all.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A virtual function that writes a contract** is reported — P2900 says it shall not.
#[test]
fn a_contract_on_a_virtual_function_is_reported() {
    let codes = codes_of(
        "struct Base {\n    virtual int compute(int x) pre(x > 0);\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::ContractOnVirtualFunction,
        "a virtual function has no contract specifiers in C++26",
    );
}

/// **A pure virtual function writing one** is the same rule — the `= 0` does not make it a different placement.
#[test]
fn a_contract_on_a_pure_virtual_function_is_reported() {
    let codes = codes_of(
        "struct Base {\n    virtual int compute(int x) pre(x > 0) = 0;\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::ContractOnVirtualFunction,
        "`= 0` is still a virtual function",
    );
}

/// **An ordinary function's contract is fine**, which is the negative that keeps the rule from firing on every
/// contract in a program.
#[test]
fn a_contract_on_an_ordinary_function_is_silent() {
    let codes = codes_of("int compute(int x) pre(x > 0) { return x; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::ContractOnVirtualFunction,
        "a non-virtual function may have a contract",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ContractOnDefaultedFunction,
        "and it is not defaulted",
    );
}

/// **A virtual function without a contract** is what almost every virtual function is: silence.
#[test]
fn a_virtual_function_without_a_contract_is_silent() {
    let codes = codes_of("struct Base {\n    virtual int compute(int x);\n};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::ContractOnVirtualFunction,
        "there is no contract to complain about",
    );
}

/// **A function defaulted on its first declaration that writes a contract** is reported.
#[test]
fn a_contract_on_a_defaulted_first_declaration_is_reported() {
    let codes = codes_of("struct Widget {\n    int size;\n    Widget() pre(true) = default;\n};\n");
    assert_reported(
        &codes,
        DiagnosticCode::ContractOnDefaultedFunction,
        "a function defaulted on its first declaration has no contract specifiers",
    );
}

/// **A defaulted function with no contract** is every defaulted function in ordinary code: silence.
#[test]
fn a_defaulted_function_without_a_contract_is_silent() {
    let codes = codes_of("struct Widget {\n    int size;\n    Widget() = default;\n};\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::ContractOnDefaultedFunction,
        "there is no contract to complain about",
    );
}

/// **A later declaration that is defaulted may carry a contract.** `Widget::Widget() = default;` written out of class
/// follows an in-class declaration, so it is not defaulted on its *first* declaration — the standard allows it, and a
/// rule that reported it would underline correct code.
#[test]
fn a_contract_on_a_later_defaulted_declaration_is_silent() {
    let codes = codes_of(
        "struct Widget {\n    Widget();\n};\nWidget::Widget() pre(true) = default;\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ContractOnDefaultedFunction,
        "an out-of-class defaulted definition is not the first declaration",
    );
}
