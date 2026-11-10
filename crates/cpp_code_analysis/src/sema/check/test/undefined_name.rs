//! **`undefined-name`** — a name read where nothing this file can see declares it.
//!
//! The rule asks two narrow places: **the object of a member access written as one bare identifier**, and **the bare
//! callee of a call**. Everything else — a keyword, a qualified spelling, an expression — is another question. The
//! negatives below are the reasons it stays quiet: a name the file declares, a keyword or an expression that is not a
//! name, a body where the declaration is not this file's to see, and a call an argument's type may find by ADL.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain receiver**: `missing.size` has no declaration to type it, which is why `undefined-field` has nothing
/// to say and this rule does.
#[test]
fn a_receiver_that_nothing_declares_is_reported() {
    let codes = codes_of("struct Widget { int size; };\nvoid f() {\n    missing.size;\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UndefinedName,
        "nothing declares `missing`",
    );
}

/// **A bare callee** is the other half of "a name is read here": nothing declares `printh`.
#[test]
fn a_callee_that_nothing_declares_is_reported() {
    let codes = codes_of("void f() {\n    printh(\"x\", 12);\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UndefinedName,
        "nothing declares `printh`",
    );
}

/// **A name the file declares is not this rule's business**: the receiver is typed, and the member access is
/// `undefined-field`'s question instead.
#[test]
fn a_name_the_file_declares_is_silent() {
    let codes =
        codes_of("struct Widget { int size; };\nvoid f() {\n    Widget w;\n    w.size;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::UndefinedName,
        "`w` is declared on the line above",
    );
}

/// **A keyword and an expression are not bare names**: `this` is not an identifier, and `(*p)` is a parenthesised
/// expression rather than the one identifier the rule asks about.
#[test]
fn a_keyword_and_an_expression_are_not_names() {
    let keyword = codes_of("struct Widget { int size; void f() { this->size = 1; } };\n");
    assert_silent_about(
        &keyword,
        DiagnosticCode::UndefinedName,
        "`this` is a keyword, not a name",
    );

    let expression = codes_of("struct Widget { int size; };\nvoid f(Widget* p) { (*p).size; }\n");
    assert_silent_about(
        &expression,
        DiagnosticCode::UndefinedName,
        "the object is an expression, not one bare name",
    );
}

/// **A dependent body and a macro are both "not read" rather than "not declared"**: a template's name may come from
/// a base that exists only at instantiation, and a macro's spelling belongs to its expansion.
#[test]
fn a_template_body_and_a_macro_are_silent() {
    let templated =
        codes_of("template <class T>\nstruct Derived : T {\n    void f() { count.size; }\n};\n");
    assert_silent_about(
        &templated,
        DiagnosticCode::UndefinedName,
        "a template body is dependent",
    );

    let macro_defined = codes_of(
        "struct Widget { int size; };\n\
         #define receiver w\n\
         void f() {\n    Widget w;\n    receiver.size;\n}\n",
    );
    assert_silent_about(
        &macro_defined,
        DiagnosticCode::UndefinedName,
        "`receiver` is a macro, and the expanded name is declared",
    );
}

/// **A lambda body and an ADL call are silence for different reasons**: a capture is a declaration the scope tree
/// does not bind, and an unqualified call may be found through the namespaces of a class-typed argument.
#[test]
fn a_lambda_body_and_an_adl_call_are_silent() {
    let lambda = codes_of(
        "struct Widget { int size; };\n\
         void f() {\n    Widget w;\n    auto g = [&w]() { w.size; };\n}\n",
    );
    assert_silent_about(
        &lambda,
        DiagnosticCode::UndefinedName,
        "a capture is not a name the scope tree holds",
    );

    let adl = codes_of("struct Widget { int size; };\nvoid f(Widget w) {\n    swap(w, w);\n}\n");
    assert_silent_about(
        &adl,
        DiagnosticCode::UndefinedName,
        "`swap` may be found through `Widget`'s namespace",
    );
}
