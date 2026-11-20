//! **Category: long braced lists as column tables** — clang-format 23's layout for a braced initializer list that does
//! not fit on one line.
//!
//! ```text
//! static const int n[] = {111, 2222, 33, 44444, 5,  666, 7777, 88, 999,
//!                         10,  11,   12, 13,    14, 15,  16,   17, 18,
//!                         19,  20,   21, 22,    23, 24,  25};
//! ```
//!
//! The layout is not an option — `AlignArrayOfStructures` is `None` in LLVM's style and setting it to `Left` or `Right`
//! does not change this output — so these fixtures are the oracle's bytes. The rules are in `tables.rs`: a column is
//! as wide as its widest cell, the row count starts from the list's total width over the room a line has, and the first
//! row stays on the `{` line unless starting it there costs at least two rows.

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

/// An integer array: nine columns, and the first row stays on the `{` line.
#[test]
fn an_integer_array_is_a_table_on_the_brace_line() {
    let input = "static const int n[] = {111, 2222, 33, 44444, 5, 666, 7777, 88, 999, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25};\n";
    let expected = "\
static const int n[] = {111, 2222, 33, 44444, 5,  666, 7777, 88, 999,
                        10,  11,   12, 13,    14, 15,  16,   17, 18,
                        19,  20,   21, 22,    23, 24,  25};
";
    check(input, expected);
}

/// The 25 operator names of `NameStyleChecker::TableFieldSpecialName`: five columns, and the table moves to the next
/// line because starting it after the `{` would cost seven rows of four.
#[test]
fn the_operator_names_are_five_by_five_on_the_next_line() {
    let mut input = String::from("std::set<std::string, std::less<>> X = {\n");
    input.push_str("        \"__add\", \"__sub\", \"__mul\", \"__div\", \"__mod\", \"__pow\",\n");
    input.push_str("        \"__unm\", \"__idiv\", \"__band\", \"__bor\", \"__bxor\", \"__bnot\", \"__shl\",\n");
    input.push_str("        \"__shr\", \"__concat\", \"__len\", \"__eq\", \"__lt\", \"__index\", \"__newindex\",\n");
    input.push_str("        \"__call\", \"__gc\", \"__close\", \"__mode\", \"__name\"};\n");
    let expected = "\
std::set<std::string, std::less<>> X = {
    \"__add\",  \"__sub\",  \"__mul\",   \"__div\",   \"__mod\",
    \"__pow\",  \"__unm\",  \"__idiv\",  \"__band\",  \"__bor\",
    \"__bxor\", \"__bnot\", \"__shl\",   \"__shr\",   \"__concat\",
    \"__len\",  \"__eq\",   \"__lt\",    \"__index\", \"__newindex\",
    \"__call\", \"__gc\",   \"__close\", \"__mode\",  \"__name\"};
";
    check(&input, expected);
}

/// A list that fits stays on one line — the table is only for a list that does not.
#[test]
fn a_fitting_list_is_one_line() {
    check(
        "static const int n[] = {1, 2, 3};\n",
        "static const int n[] = {1, 2, 3};\n",
    );
}

/// **A braced list that is a call's argument is wrapped by the argument rules, not laid out as a table.** clang-format
/// (`LLVM`) writes this fixture byte for byte: the temporary `frame{…}` wraps as a plain list aligned after the `(`,
/// while a declaration's initializer (`std::vector<int> v{…}`, `= {…}`, `std::array<int, 15>{…}`) is a table.
#[test]
fn a_braced_argument_is_not_a_table() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "void f() {\n  frames.push_back(frame{nullptr, nullptr, slot, elements.size(),\n                         ordered_state(dest_member, false)});\n  std::vector<int> v{111, 2222, 33, 44444, 5,  666, 7777, 88,\n                     999, 10,   11, 12,    13, 14,  15};\n}\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}
