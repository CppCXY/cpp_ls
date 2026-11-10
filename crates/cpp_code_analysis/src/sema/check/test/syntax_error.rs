//! **`syntax-error`** — the file does not parse, and the parser said where.
//!
//! This is the one check with no `Known` question behind it: it republishes the parser's own errors, in the order
//! they were found, and filters none of them. So the negatives here are not about the rule's judgement — it has none
//! — but about what a parse error is not: a comment, a string literal, or a macro directive is not code the parser
//! ever tries to read as a statement.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain case**: a parameter list that is never closed cannot parse.
#[test]
fn an_unclosed_parameter_list_is_reported() {
    let codes = codes_of("int main( { return 0; }\n");
    assert_reported(
        &codes,
        DiagnosticCode::SyntaxError,
        "the `(` of `main` is never closed, so the file does not parse",
    );
}

/// **A file that parses produces no parse error** — the assertion that keeps the rule from reporting recovery noise.
#[test]
fn a_file_that_parses_is_quiet() {
    let plain = codes_of("int add(int a, int b) { return a + b; }\n");
    assert_silent_about(
        &plain,
        DiagnosticCode::SyntaxError,
        "a plain function is a program the parser reads without complaint",
    );

    let declarations = codes_of("struct Point { int x; int y; };\nPoint origin;\n");
    assert_silent_about(
        &declarations,
        DiagnosticCode::SyntaxError,
        "a class and a variable are ordinary declarations",
    );
}

/// **A comment is trivia**, so an unbalanced brace written in one never opens or closes anything.
#[test]
fn a_comment_is_not_part_of_the_program() {
    let commented_out = codes_of("// a brace that opens nothing: {\nint f() { return 0; }\n");
    assert_silent_about(
        &commented_out,
        DiagnosticCode::SyntaxError,
        "a `{` inside a comment is trivia and the parser never sees a brace",
    );
}

/// **A string literal is one token**, so a brace inside it is data rather than a block the parser must close.
#[test]
fn a_string_literal_holds_no_syntax() {
    let braced = codes_of("const char* text = \"{\";\nint f() { return 0; }\n");
    assert_silent_about(
        &braced,
        DiagnosticCode::SyntaxError,
        "the `{` is inside a string literal, which is one token and not a brace",
    );
}

/// **A macro definition is a directive**, and its body is not a statement the parser reads on its own.
#[test]
fn a_macro_definition_is_not_a_statement() {
    let defined = codes_of("#define TWICE(x) ((x) * 2)\nint f() { return TWICE(2); }\n");
    assert_silent_about(
        &defined,
        DiagnosticCode::SyntaxError,
        "a `#define` is a directive, and its body is not C++ the parser parses as code",
    );
}
