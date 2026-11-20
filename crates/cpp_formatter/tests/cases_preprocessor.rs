//! **Category: preprocessor** — inline cases, no files, no clang-format at test time.
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

/// `01_define`
#[test]
fn preprocessor_01_define() {
    check(
        r#"#define MAX(a, b) ((a) > (b) ? (a) : (b))"#,
        r#"#define MAX(a, b) ((a) > (b) ? (a) : (b))"#,
    );
}

/// `02_if`
#[test]
fn preprocessor_02_if() {
    check(
        r#"#if DEBUG
int debug = 1;
#endif"#,
        r#"#if DEBUG
int debug = 1;
#endif"#,
    );
}

/// `03_include`
#[test]
fn preprocessor_03_include() {
    check(
        r#"#include <vector>
#include "local.h""#,
        r#"#include "local.h"
#include <vector>"#,
    );
}

/// `04_multiline`
#[test]
fn preprocessor_04_multiline() {
    check(
        r#"#define M(a) \
    do { \
        (void)(a); \
    } while (0)"#,
        r#"#define M(a)       \
    do {           \
        (void)(a); \
    } while (0)"#,
    );
}

/// `05_pragma`
#[test]
fn preprocessor_05_pragma() {
    check(r#"#pragma once"#, r#"#pragma once"#);
}

/// `06_undef`
#[test]
fn preprocessor_06_undef() {
    check(r#"#undef FOO"#, r#"#undef FOO"#);
}

/// `07_define_alignment` — **`AlignEscapedNewlines: Right`**: every escaped line of a macro ends at the column limit,
/// which is clang-format 23.1.5's answer for `LLVM` (the style here has `ColumnLimit: 0`, where `Right` falls back to
/// `Left` — so this case uses a style with a limit).
#[test]
fn preprocessor_07_define_alignment() {
    let mut styled = style();
    styled.set_from_text("ColumnLimit", "80");
    let input = "#define BOOL_OPTION(op) \\\n  if (auto n = root.GetValue(#op); n.IsBool()) \\\n  { \\\n    op = n.AsBool(); \\\n  }\n";
    let expected = "#define BOOL_OPTION(op)                                                        \\\n  if (auto n = root.GetValue(#op); n.IsBool())                                 \\\n  {                                                                            \\\n    op = n.AsBool();                                                           \\\n  }\n";
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &styled);
    assert_eq!(actual, expected, "the backslashes end at column 80");
    let again = cpp_formatter::format_file(path, expected, &styled);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// **A bare macro invocation is a line of its own**: `NLOHMANN_JSON_NAMESPACE_BEGIN` has no `;`, and the reflow used to
/// join it with the declaration after it (`… _BEGIN namespace detail {`, measured on nlohmann json).
#[test]
fn preprocessor_08_a_bare_macro_line() {
    check(
        "NLOHMANN_JSON_NAMESPACE_BEGIN\nnamespace detail {\nvoid f();\n}\n",
        "NLOHMANN_JSON_NAMESPACE_BEGIN\nnamespace detail {\nvoid f();\n}\n",
    );
}

/// **A directive is never a continuation of the line above**: `#include <cstdlib>` ends with a `>` that reads as a
/// binary operator, and the `#define` after it was aligned 23 columns in (measured on nlohmann json's
/// `macro_scope.hpp`, where `IndentPPDirectives: None` wants column 0).
#[test]
fn preprocessor_09_a_define_after_an_include() {
    check(
        "#if defined(A)\n#define THROW(e) throw e\n#else\n#include <cstdlib>\n#define THROW(e) std::abort()\n#endif\n",
        "#if defined(A)\n#define THROW(e) throw e\n#else\n#include <cstdlib>\n#define THROW(e) std::abort()\n#endif\n",
    );
}

/// **A spliced line break inside a `#define` keeps its line**: the `\` and the break are the macro's, and the pass had
/// no line break to emit for them, so three spliced lines used to be joined into one (measured on nlohmann json's
/// `macro_scope.hpp`, whose `NLOHMANN_JSON_SERIALIZE_ENUM` macro is 60 spliced lines). clang-format 23.1.5 writes this
/// fixture exactly like the expected side.
#[test]
fn preprocessor_10_a_spliced_macro_keeps_its_lines() {
    check(
        "#define TWICE(x)   \\\n    int y = (x);   \\\n    use(y)\n",
        "#define TWICE(x) \\\n    int y = (x); \\\n    use(y)\n",
    );
}

/// **A directive after a trailing comment still goes to column 0**: the gap holds the previous line's comment, and the
/// verbatim copy used to keep the *source's* indentation of the directive (`    #include <string>` stayed four columns
/// in, measured on nlohmann json's `json_pointer.hpp`, where the includes sit indented inside a namespace). clang-format
/// 23.1.5 (`LLVM`) writes this fixture byte for byte.
#[test]
fn preprocessor_11_a_directive_after_a_trailing_comment() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "int x; // note\n#include <iosfwd>\n#include <string>\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}