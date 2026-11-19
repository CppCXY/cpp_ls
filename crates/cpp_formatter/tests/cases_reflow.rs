//! **Category: the reflow's join half** — a statement that fits on one line is one line, against clang-format 23.1.5.
//!
//! clang-format re-lays-out every logical line: when the whole statement fits, the author's breaks inside it are
//! removed (`int x = foo(a,\n            b);` -> `int x = foo(a, b);`). This formatter keeps the author's breaks, which
//! is why a file written by another formatter differed on every wrapped expression even once the answer was known.
//!
//! The cases that must **not** join are the interesting ones, and each is here: a statement with a comment, a
//! statement with a brace (`BinPackLongBracedList: false` keeps a long braced list broken), a run of macro invocations
//! with no terminator, a label's statement, and a statement that does not fit.

use cpp_formatter::config::Style;

fn style() -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("IndentWidth", "2");
    style.set_from_text("ContinuationIndentWidth", "4");
    style
}

fn check(input: &str, expected: &str) {
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &style());
    assert_eq!(
        actual, expected,
        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style());
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// A fitting call joins — clang-format's answer for both shapes.
#[test]
fn a_fitting_statement_joins() {
    check(
        "void f() {\n  int x = foo(a,\n              b);\n  auto y = obj.method(1,\n                      2);\n}\n",
        "void f() {\n  int x = foo(a, b);\n  auto y = obj.method(1, 2);\n}\n",
    );
}

/// A statement that does not fit keeps the author's lines (the breaking half is not implemented: `break_line`'s model
/// is incomplete, measured in `docs/formatter-diff.md`).
#[test]
fn a_statement_that_does_not_fit_keeps_its_lines() {
    let input = "void f() {\n  int x = some_function_with_a_long_name(first_argument, second_argument,\n                                         third_argument, fourth_argument);\n}\n";
    check(input, input);
}

/// A **comment** in the statement is the author's line: `// why` keeps its break.
#[test]
fn a_statement_with_a_comment_is_not_joined() {
    let input = "void f() {\n  int x = foo(a, // why\n              b);\n}\n";
    check(input, input);
}

/// A **brace** keeps the statement broken (`BinPackLongBracedList: false`), and a block body never joins.
#[test]
fn a_statement_with_a_brace_is_not_joined() {
    let input = "int values[] = {1,\n                2};\n";
    check(input, input);
}

/// A **run of macro invocations** has no terminator; joining it put two statements on one line.
#[test]
fn a_run_of_macro_invocations_is_not_joined() {
    let input = "void f() {\n  NUMBER_OPTION(a)\n  NUMBER_OPTION(b)\n}\n";
    check(input, input);
}

/// A **label's statement** keeps its line: `endLoop:` then `return words;` are two lines.
#[test]
fn a_labeled_statement_is_not_joined() {
    let input = "int f() {\nendLoop:\n  return words;\n}\n";
    check(input, input);
}
