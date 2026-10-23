//! **Category: statements** — inline cases, no files, no clang-format at test time.
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

/// `05_return`
#[test]
fn statements_05_return() {
    check(
        r#"int f() { return; }"#,
        r#"int f() { return; }"#,
    );
}

/// `07_if_block`
#[test]
fn statements_07_if_block() {
    check(
        r#"void f(int x) {
    if (x>0) {
        g();
    } else {
        h();
    }
}"#,
        r#"void f(int x) {
    if (x > 0) {
        g();
    } else {
        h();
    }
}"#,
    );
}

/// `08_for_block`
#[test]
fn statements_08_for_block() {
    check(
        r#"void f() {
    for (int i=0; i<3; ++i) {
        g(i);
    }
}"#,
        r#"void f() {
    for (int i = 0; i < 3; ++i) {
        g(i);
    }
}"#,
    );
}

/// `09_switch_block`
#[test]
fn statements_09_switch_block() {
    check(
        r#"void f(int k) {
    switch (k) {
    case 1:
        break;
    default:
        break;
    }
}"#,
        r#"void f(int k) {
    switch (k) {
    case 1:
        break;
    default:
        break;
    }
}"#,
    );
}

/// `10_while_block`
#[test]
fn statements_10_while_block() {
    check(
        r#"void f() {
    while (x>0) {
        --x;
    }
}"#,
        r#"void f() {
    while (x > 0) {
        --x;
    }
}"#,
    );
}

/// `11_return_block`
#[test]
fn statements_11_return_block() {
    check(
        r#"int f() {
    return  0;
}"#,
        r#"int f() {
    return 0;
}"#,
    );
}

/// `12_ternary_block`
#[test]
fn statements_12_ternary_block() {
    check(
        r#"int f(int a) {
    return a>0 ? a : -a;
}"#,
        r#"int f(int a) {
    return a > 0 ? a : -a;
}"#,
    );
}
