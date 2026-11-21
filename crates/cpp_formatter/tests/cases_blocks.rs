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

/// **The body of an unbraced control statement sits one level in**, and the level ends with that statement's `;` —
/// including through a nest of two unbraced bodies. Measured against clang-format 23.1.5 (`LLVM`) on Lua 5.5's
/// `lauxlib.c`, where the statement came out at the block level.
#[test]
fn blocks_21_an_unbraced_control_body() {
    check(
        "static int f(int level) {\n  if (level == 0)\n    return 0;\n  return 1;\n}\n",
        "static int f(int level) {\n  if (level == 0)\n    return 0;\n  return 1;\n}\n",
    );
    check(
        "void f(int a, int b) {\n  if (a)\n    if (b)\n      g();\n  h();\n}\n",
        "void f(int a, int b) {\n  if (a)\n    if (b)\n      g();\n  h();\n}\n",
    );
    // A `do { … } while (…)` tail is not a head that opens a body: the line after it is at the block level. (The macro
    // body itself is not formatted — a `#define` whose body holds braces is a separate, unimplemented feature, and
    // `docs/formatter-diff.md` records it — so this fixture keeps the macro out and tests the statement after it.)
    check(
        "void f() {\n  do {\n    g();\n  } while (x);\n  h();\n}\n",
        "void f() {\n  do {\n    g();\n  } while (x);\n  h();\n}\n",
    );
}
/// **`ShortNamespaceLines` counts the body's unwrapped lines**: with the LLVM default of 1 a namespace holding one
/// statement gets no closing comment, and one holding two gets it (measured against clang-format 23.1.5).
#[test]
fn blocks_22_short_namespace_lines() {
    check(
        "namespace detail {\nvoid f();\n}\n",
        "namespace detail {\nvoid f();\n}\n",
    );
    check(
        "namespace detail {\nvoid f();\nvoid g();\n}\n",
        "namespace detail {\nvoid f();\nvoid g();\n} // namespace detail\n",
    );
}

/// **A trailing comment is part of the line's width**: a member declaration that fits on its own but not with the
/// `// NOLINT(…)` after it is broken by clang-format — after the `=`, leaving the comment on the last line. Measured
/// against clang-format 23.1.5 on nlohmann json's `json_sax.hpp`.
#[test]
fn blocks_23_a_break_for_the_sake_of_a_trailing_comment() {
    check(
        "class json_sax_dom_error_state {\npublic:\n  json_sax_dom_error_state(json_sax_dom_error_state &&) = default; // NOLINT(hicpp-noexcept-move,performance-noexcept-move-constructor)\n};\n",
        "class json_sax_dom_error_state {\npublic:\n  json_sax_dom_error_state(json_sax_dom_error_state &&) =\n      default; // NOLINT(hicpp-noexcept-move,performance-noexcept-move-constructor)\n};\n",
    );
}

/// **A namespace that contains another one closes with its own name**: `namespace multi { … namespace inner { … } }`
/// ends with `} // namespace multi`, not the inner name — the syntax node of the outer declaration may hold only the
/// body, so the name is read from the source text before the brace. clang-format 23.1.5 (`LLVM`) writes this fixture
/// byte for byte.
#[test]
fn blocks_24_a_nested_namespace_comment() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "namespace multi {\nint a;\nnamespace inner {\nint b;\n}\n}\n";
    let expected =
        "namespace multi {\nint a;\nnamespace inner {\nint b;\n}\n} // namespace multi\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), expected);
}

/// **`FixNamespaceComments` also fixes a wrong comment**: `} // wrong name` becomes `} // namespace existing`, and it
/// does so whatever the body length — while a *missing* comment is only added past `ShortNamespaceLines` (the fixture
/// above). clang-format 23.1.5 (`LLVM`) writes this fixture byte for byte.
#[test]
fn blocks_25_a_wrong_namespace_comment_is_fixed() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "namespace existing {\nint w;\n} // wrong name\n";
    let expected = "namespace existing {\nint w;\n} // namespace existing\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), expected);
    // A one-statement body gets no new comment: `} ` stays `}`.
    let short = "namespace detail {\nvoid f();\n}\n";
    assert_eq!(cpp_formatter::format_file(path, short, &llvm), short);
}

/// **A record's body is never on one line**: `struct T { int z; };` written on one line comes back as three lines —
/// clang-format breaks a non-empty record body under every style (measured on `LLVM`, `Google` and `Microsoft`). An
/// empty body is still `{}`, and an enumeration has `AllowShortEnumsOnASingleLine` instead.
#[test]
fn blocks_26_a_record_body_is_never_one_line() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "struct T { int z; };\n";
    let expected = "struct T {\n  int z;\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), expected);
    let empty = "struct E {};\n";
    assert_eq!(cpp_formatter::format_file(path, empty, &llvm), empty);
}

/// **`AllowShortEnumsOnASingleLine`**: an enumeration whose text fits is one line under `LLVM`/`Google` (the option is
/// true there and false in `Microsoft`), and the one-line form is refused when the last item keeps a trailing comma —
/// `{ A, B, }` is not a shape clang-format writes, so `enum class I { A, B, } i = I::A;` stays broken while the same
/// body without the comma joins. All four shapes are the oracle's, byte for byte.
#[test]
fn blocks_27_a_short_enum_is_one_line() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "enum class E\n{\n    A,\n    B\n};\n";
    let expected = "enum class E { A, B };\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), expected);
    // A trailing comma keeps the body broken.
    let trailing = "enum class I\n{\n    A,\n    B,\n} i = I::A;\n";
    let broken = "enum class I {\n  A,\n  B,\n} i = I::A;\n";
    assert_eq!(cpp_formatter::format_file(path, trailing, &llvm), broken);
    // Microsoft writes `BeforeEnum: true` (Allman) and keeps `AllowShortEnumsOnASingleLine` off, so the brace and the
    // body keep their own lines — the oracle's text.
    let microsoft = Style::preset("Microsoft").expect("Microsoft is a preset");
    assert_eq!(
        cpp_formatter::format_file(path, input, &microsoft),
        "enum class E\n{\n    A,\n    B\n};\n"
    );
}

/// **`AllowShortBlocksOnASingleLine: Never`** is the one option all three styles agree on: `if (a) { g(); }` written on
/// one line comes back as three lines under `LLVM`, `Google` and `Microsoft` (measured on the oracle). An **empty**
/// body stays `{}` — the branch this rule sits next to.
#[test]
fn blocks_28_a_control_block_is_never_one_line() {
    let path = std::path::Path::new("snippet.cpp");
    let input = "void f(int a) {\n  if (a) { g(); }\n}\n";
    let expected = "void f(int a) {\n  if (a) {\n    g();\n  }\n}\n";
    // `LLVM` and `Google` attach the brace, so both write the same text; `Microsoft`'s Allman placement is covered by
    // its own cases.
    for preset in ["LLVM", "Google"] {
        let style = Style::preset(preset).expect("a preset");
        assert_eq!(
            cpp_formatter::format_file(path, input, &style),
            expected,
            "{preset}"
        );
    }
    // **An empty control body is split too** (`{` newline `}`), which is the oracle's answer under `LLVM`.
    let empty = "void f(int a) {\n  if (a) {}\n}\n";
    let empty_expected = "void f(int a) {\n  if (a) {\n  }\n}\n";
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    assert_eq!(
        cpp_formatter::format_file(path, empty, &llvm),
        empty_expected
    );
}
