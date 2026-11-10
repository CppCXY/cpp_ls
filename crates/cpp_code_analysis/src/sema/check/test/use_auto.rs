//! **`use-auto`** — an iterator declared by its full spelling, where `auto` names the same type by deduction.
//!
//! The rule is deliberately narrow: a declaration whose **type node's own text** ends in `::iterator` or
//! `::const_iterator` and which carries one plain declarator name. Nothing else is its shape — the same characters in
//! a comment, in a string, or in a parameter that declares no name are all silence, because the question is about a
//! declaration rather than about a spelling found in the file.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The spelling every standard container defines**, and the one the rule claims.
#[test]
fn a_full_iterator_spelling_is_reported() {
    let codes = codes_of("void f() {\n    std::vector<int>::iterator it;\n    (void)it;\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UseAuto,
        "`it` is declared by its full iterator spelling",
    );
}

/// **`const_iterator` is the same shape**, with the const written into the name.
#[test]
fn a_full_const_iterator_spelling_is_reported() {
    let codes =
        codes_of("void f() {\n    std::vector<int>::const_iterator it;\n    (void)it;\n}\n");
    assert_reported(
        &codes,
        DiagnosticCode::UseAuto,
        "`it` is declared by its full `::const_iterator` spelling",
    );
}

/// **The declaration is the question, not the word `iterator`**: a different member type and a deduced type are both
/// outside the rule.
#[test]
fn another_member_type_and_auto_are_silent() {
    let value_type =
        codes_of("void f() {\n    std::vector<int>::value_type value;\n    (void)value;\n}\n");
    assert_silent_about(
        &value_type,
        DiagnosticCode::UseAuto,
        "`value_type` is not an iterator",
    );

    let deduced = codes_of("void f() {\n    auto it = 0;\n    (void)it;\n}\n");
    assert_silent_about(
        &deduced,
        DiagnosticCode::UseAuto,
        "`auto` is the answer, not the question",
    );
}

/// **A declarator that is not one plain name is not a declaration `auto` can replace**: `items[4]` declares an
/// array, and an unnamed parameter declares no iterator at all.
#[test]
fn a_declarator_that_is_not_one_name_is_silent() {
    let array = codes_of("void f() {\n    std::vector<int>::iterator items[4];\n}\n");
    assert_silent_about(
        &array,
        DiagnosticCode::UseAuto,
        "`items[4]` is an array, not one name",
    );

    let parameter = codes_of("void g(std::vector<int>::iterator);\n");
    assert_silent_about(
        &parameter,
        DiagnosticCode::UseAuto,
        "the parameter declares no name to rewrite",
    );
}

/// **A comment and a string are not declarations** — the rule reads the declaration's type node, so the same
/// characters outside a declaration say nothing.
#[test]
fn prose_and_data_are_not_declarations() {
    let commented = codes_of("// std::vector<int>::iterator it;\nvoid f() { }\n");
    assert_silent_about(
        &commented,
        DiagnosticCode::UseAuto,
        "a comment is not a declaration",
    );

    let quoted = codes_of(
        "void g(const char*);\nvoid f() {\n    g(\"std::vector<int>::iterator it;\");\n}\n",
    );
    assert_silent_about(
        &quoted,
        DiagnosticCode::UseAuto,
        "a string literal is not a declaration",
    );
}
