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

/// **A `&&` in a control statement's condition is an operator, not a declarator**: the parser reads
/// `if (isNameDefine && CheckGlobal(x, t))` as a declaration of a function returning `isNameDefine&&`, and a
/// declarator mark there made the formatter write `&&CheckGlobal` (measured on `NameStyleChecker.cpp`). The tree is
/// consulted only for the condition itself — a declaration inside the body keeps its marks.
#[test]
fn spacing_15_logical_and_in_a_condition() {
    check(
        "void f() {\n    if (isNameDefine &&CheckGlobal(x, t)) {\n        g();\n    }\n}\n",
        "void f() {\n    if (isNameDefine && CheckGlobal(x, t)) {\n        g();\n    }\n}\n",
    );
    check(
        "void f() {\n    const auto &style = g();\n    (void)style;\n}\n",
        "void f() {\n    const auto &style = g();\n    (void)style;\n}\n",
    );
}

/// **A template argument list's `>` glues to the name it closes over**: `std::numeric_limits<size_t>::max` is one
/// name. The operator rule ("a space before `::` after an operator") put one in it.
#[test]
fn spacing_16_scope_after_a_template_argument_list() {
    check(
        "void f() {\n    (std::numeric_limits<std::size_t>::max)();\n    auto x = std::vector<int>::size_type{};\n}\n",
        "void f() {\n    (std::numeric_limits<std::size_t>::max)();\n    auto x = std::vector<int>::size_type{};\n}\n",
    );
}

/// **A `*` after a builtin type name is a declarator even where the tree does not say so**: a `for` statement's init
/// declaration is not annotated, so both sides got binary-operator spaces (`const char * p`, measured on nlohmann
/// json's `number_parse.hpp`). The rule is `PointerAlignment`'s, as for every other declarator.
#[test]
fn spacing_17_a_pointer_in_a_for_init() {
    check(
        "void f() {\n    for (const char *p = first; p != last; ++p) {\n        g(p);\n    }\n}\n",
        "void f() {\n    for (const char *p = first; p != last; ++p) {\n        g(p);\n    }\n}\n",
    );
    // A cast's `*` and a multiplication are not declarators: `a * b` keeps its spaces.
    check(
        "void f(int a, int b) {\n    int c = a * b;\n}\n",
        "void f(int a, int b) {\n    int c = a * b;\n}\n",
    );
}

/// **A conversion operator keeps the space after `operator`**: `operator size_t()` is a type name, not punctuation,
/// while `operator=`, `operator==` and `operator[]` stay glued. The rule used to glue every token after `operator`,
/// which wrote `operatorsize_t()` — a merge that made the whole file fall back to its source (the renderer verifies the
/// token stream). Measured on nlohmann json's `position_t.hpp`; clang-format 23.1.5 writes the fixture byte for byte.
#[test]
fn spacing_18_a_conversion_operator_keeps_its_space() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "struct S {\n  operator size_t() const { return 2; }\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
    let glued = "struct S {\n  bool operator==(const S &other) const { return x == other.x; }\n  int operator[](int at) const { return x; }\n};\n";
    assert_eq!(cpp_formatter::format_file(path, glued, &llvm), glued);
}
