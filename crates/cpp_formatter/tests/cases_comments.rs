//! **Category: comments** — inline cases, no files, no clang-format at test time.
//!
//! Each case is a pair of strings: the input as written, and the text this formatter must produce for it. The pairs
//! were taken from clang-format 22.1.3 for the same style that `style()` documents, so the expected side is the
//! oracle's answer rather than whatever this implementation happened to print.

use cpp_formatter::config::Style;

/// Format `input` with this project's style and compare against the oracle's answer, then re-format the answer.
///
/// The second half is the property a user feels: formatting text that is already formatted has to leave it alone.
#[track_caller]
fn check(input: &str, expected: &str) {
    let style = style();
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &style);
    // The trailing newline is the *source's* (the formatter preserves its absence), so the cases compare the
    // content and leave the file's final byte to `trim_end_matches` on both sides.
    let actual = actual.trim_end_matches('\n');
    let expected = expected.trim_end_matches('\n');
    assert_eq!(
        actual, expected,
        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// The style the expected strings were produced with.
fn style() -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("IndentWidth", "4");
    style.set_from_text("ContinuationIndentWidth", "8");
    style.set_from_text("ColumnLimit", "0");
    style
}

/// `01_trailing_line`
#[test]
fn comments_01_trailing_line() {
    check(
        r#"int x = 1; // note"#,
        r#"int x = 1; // note"#,
    );
}

/// `02_trailing_block`
#[test]
fn comments_02_trailing_block() {
    check(
        r#"int x = 1; /* note */"#,
        r#"int x = 1; /* note */"#,
    );
}

/// `03_own_line`
#[test]
fn comments_03_own_line() {
    check(
        r#"// heading
int x = 1;"#,
        r#"// heading
int x = 1;"#,
    );
}

/// `04_between`
#[test]
fn comments_04_between() {
    check(
        r#"void f() {
    // step
    g();
}"#,
        r#"void f() {
    // step
    g();
}"#,
    );
}

/// `05_doc`
#[test]
fn comments_05_doc() {
    check(
        r#"/// doc
int f();"#,
        r#"/// doc
int f();"#,
    );
}

/// `06_string`
#[test]
fn comments_06_string() {
    check(
        r#"const char* s = "// not a comment";"#,
        r#"const char *s = "// not a comment";"#,
    );
}
