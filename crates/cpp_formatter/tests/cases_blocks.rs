//! **Category: blocks and empty bodies** — the shapes every file has, taken from clang-format 15.0.1 (the copy Visual
//! Studio ships) and re-checked against it with `tests/against_clang_format.rs`.
//!
//! The rule under test is small and universal: **an empty block is `{}`**, on one line, under every style. clang-format
//! writes `void f() {}` for an empty body even when `AllowShortBlocksOnASingleLine` is `Never` — that option governs a
//! block with *statements* in it — and `struct S {};` for a record with nothing in it. Before this case existed the
//! formatter kept the programmer's two lines, which is the first difference on almost every file.

use cpp_formatter::config::Style;

/// Format `input` with this project's style and compare against the oracle's answer, then re-format the answer.
#[track_caller]
fn check(input: &str, expected: &str) {
    let style = style();
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

/// An empty control-statement body: `AllowShortBlocksOnASingleLine: Never` does **not** apply to a block with nothing
/// in it.
#[test]
fn an_empty_if_body_is_one_pair_of_braces() {
    check(
        "void f() {\n  if (x) {\n  }\n}\n",
        "void f() {\n  if (x) {}\n}\n",
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
