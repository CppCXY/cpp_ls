//! **Category: blocks and empty bodies** — the shapes every file has, taken from clang-format 23.1.5 and re-checked
//! against it with `tests/against_clang_format.rs`.
//!
//! Two rules, and they are not the same rule:
//!
//! * an empty **function, lambda, record or namespace** body is `{}` — `void f() {}`, `struct S {};` — unless the
//!   style splits it (`SplitEmptyFunction`/`Record`/`Namespace`: Microsoft's `void f()\n{\n}`, `Allman`'s
//!   `struct S\n{\n};`);
//! * an empty **control** body is `{\n}` in every style — `if (x) {` and a closing brace on its own line even under
//!   `Attach`, because `AllowShortBlocksOnASingleLine: Never` splits the block. Before these cases existed the
//!   formatter kept the programmer's two lines everywhere, which was the first difference on almost every file.

use cpp_formatter::config::Style;

/// Format `input` with this project's style and compare against the oracle's answer, then re-format the answer.
#[track_caller]
fn check(input: &str, expected: &str) {
    check_with(style(), input, expected);
}

/// The same, with the style spelled by the case — for the answers that depend on an option.
#[track_caller]
fn check_with(style: Style, input: &str, expected: &str) {
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &style);
    let actual = actual.trim_end_matches('\n');
    let expected = expected.trim_end_matches('\n');
    assert_eq!(
        actual, expected,
        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

fn style() -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("IndentWidth", "2");
    style.set_from_text("ContinuationIndentWidth", "4");
    style
}

/// `void f() {\n}` is `void f() {}` — the case that was wrong on every file.
#[test]
fn an_empty_function_body_is_one_pair_of_braces() {
    check("void f() {\n}\n", "void f() {}\n");
}

/// The same inside a class, where the body is the constructor's.
#[test]
fn an_empty_constructor_body_is_one_pair_of_braces() {
    check(
        "class T {\npublic:\n  T() {\n  }\n};\n",
        "class T {\npublic:\n  T() {}\n};\n",
    );
}

/// **An empty control-statement body is `{\n}`, not `{}`** — measured against clang-format 23.1.5: LLVM writes
/// `if (x) {` with the closing brace on its own line, because `AllowShortBlocksOnASingleLine: Never` splits the block,
/// while an empty *function* body in the same style is `void f() {}`. The two families are decided separately (the
/// function one by `SplitEmptyFunction`).
#[test]
fn an_empty_if_body_is_a_brace_pair_on_two_lines() {
    check(
        "void f() {\n  if (x) {\n  }\n}\n",
        "void f() {\n  if (x) {\n  }\n}\n",
    );
    check(
        "void f() {\n  if (x) {}\n}\n",
        "void f() {\n  if (x) {\n  }\n}\n",
    );
}

/// A record with nothing in it.
#[test]
fn an_empty_record_is_one_pair_of_braces() {
    check("struct S {\n};\n", "struct S {};\n");
}

/// **A block with a comment is not empty.** `{ /* nothing */ }` keeps the comment's own line; collapsing it would move
/// text the reader wrote.
#[test]
fn a_block_holding_a_comment_keeps_its_lines() {
    let input = "void f() {\n  /* nothing */\n}\n";
    let formatted =
        cpp_formatter::format_file(std::path::Path::new("snippet.cpp"), input, &style());
    assert!(
        formatted.contains("{\n  /* nothing */\n}"),
        "a comment is not an empty block: {formatted:?}"
    );
}

/// **`ColumnLimit: 0` is "do not reflow"**: an empty body the author wrote as `{` newline `}` keeps that shape, where
/// an 80-column style joins it into `{}`. Measured on the corpus: clang-format 23.1.5 keeps every empty body split in
/// text it formats with that setting, and joining them was thousands of spacing-only lines.
#[test]
fn without_a_column_limit_an_empty_body_keeps_the_authors_lines() {
    let mut unlimited = style();
    unlimited.set_from_text("ColumnLimit", "0");
    check_with(unlimited.clone(), "void f() {\n}\n", "void f() {\n}\n");
    check_with(unlimited, "void f() {}\n", "void f() {}\n");
}
