//! **Category: spacing** — inline cases, no files, no clang-format at test time.
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

/// `01_call`
#[test]
fn spacing_01_call() {
    check(r#"int x=f(a,b);"#, r#"int x = f(a, b);"#);
}

/// `02_pointer`
#[test]
fn spacing_02_pointer() {
    check(r#"int *p = &x;"#, r#"int *p = &x;"#);
}

/// `03_cast`
#[test]
fn spacing_03_cast() {
    check(r#"void f() { (void)i; }"#, r#"void f() { (void)i; }"#);
}

/// `04_operators`
#[test]
fn spacing_04_operators() {
    check(r#"int y = a+b*c-d/e;"#, r#"int y = a + b * c - d / e;"#);
}

/// `05_compare`
#[test]
fn spacing_05_compare() {
    check(
        r#"bool ok = a==b && c!=d;"#,
        r#"bool ok = a == b && c != d;"#,
    );
}

/// `06_template_args`
#[test]
fn spacing_06_template_args() {
    check(
        r#"std::map<int,std::string> m;"#,
        r#"std::map<int, std::string> m;"#,
    );
}

/// `07_angle_space`
#[test]
fn spacing_07_angle_space() {
    check(r#"std::vector<int> v;"#, r#"std::vector<int> v;"#);
}

/// `08_lambda_bracket`
#[test]
fn spacing_08_lambda_bracket() {
    check(
        r#"auto f =[&](int a){ return a; };"#,
        r#"auto f = [&](int a) { return a; };"#,
    );
}

/// `09_braced_list`
#[test]
fn spacing_09_braced_list() {
    check(r#"int a[3] = {1,2,3};"#, r#"int a[3] = {1, 2, 3};"#);
}

/// `10_ternary`
#[test]
fn spacing_10_ternary() {
    check(r#"int t = a ? b : c;"#, r#"int t = a ? b : c;"#);
}
