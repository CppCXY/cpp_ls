//! **The rendering's separator rule** — layout a parser can read back.
//!
//! A rendering is built by spelling the cooked tokens out in order, separated by a space **unless** joining them would
//! change the first token (so `::` and `std` stay touching, `+` and `+` do not). The rule exists because the parser
//! reads `&str`: a `>` `>` pair has to stay joinable, and a qualified name must not acquire spaces inside it.
//!
//! The failure this file was written for: `static` followed by a macro that expands to `constexpr` rendered as
//! `staticconstexpr`, which no parser can read and no C++ compiler would accept.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::{cook_with, cook_with_file};

/// The rendered text of a source, through the same path a cook takes.
fn rendered(source: &str) -> String {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    cook_with(source, &tokens, &nothing_defined).render().text
}

/// **A macro that expands to one keyword next to a source keyword** — the measured shape: doctest's
/// `static DOCTEST_CONSTEXPR int consume(...)`.
#[test]
fn a_keyword_next_to_a_macro_that_expands_to_one_stays_two_tokens() {
    let text = rendered(
        "#define DOCTEST_CONSTEXPR constexpr\nstatic DOCTEST_CONSTEXPR int consume(const int*, int) noexcept;\n",
    );
    assert!(text.contains("static constexpr int"), "{text}");
    assert!(!text.contains("staticconstexpr"), "{text}");
}

/// The same pair when both halves come from file text — a space in the source is enough.
#[test]
fn two_source_keywords_keep_their_space() {
    let text = rendered("static constexpr int consume(const int*, int) noexcept;\n");
    assert!(text.contains("static constexpr int"), "{text}");
}

/// **A callee named by an expansion, called by tokens written after it, is scanned without its caller painted.**
///
/// The standard rescans a replacement list with the macro being replaced unavailable, but it says so about the
/// replacement list — *"not including the rest of the source file's preprocessing tokens"* (C11 6.10.3.4). Here
/// `TOP`'s body ends in a **name** (`E(BODY(D_, …)(…))` → `D_MEMBERS`), and the argument list that calls it is written
/// **after** the replacement; `D_MEMBERS`'s body needs `E`, which is still being replaced. Treating the tail as part of
/// the replacement left the inner `E` calls unexpanded — which is json's
/// `NLOHMANN_DEFINE_TYPE_INTRUSIVE`, and its 60 syntax errors (`docs/worklog.md`).
#[test]
fn a_callee_named_by_an_expansion_is_scanned_without_its_caller_painted() {
    let text = rendered(
        "#define E(x) x\n\
         #define GET(_1, _2, NAME, ...) NAME\n\
         #define BODY(Prefix, ...) E(GET(__VA_ARGS__, Prefix##MEMBERS, Prefix##EMPTY, SENTINEL))\n\
         #define D_MEMBERS(T, a) E(E(TO(a)))\n\
         #define D_EMPTY(T)\n\
         #define TO(a) a##_ok ;\n\
         #define TOP(...) E(BODY(D_, __VA_ARGS__)(__VA_ARGS__))\n\
         TOP(zz_type, zz_a)\n",
    );
    assert_eq!(text.trim(), "zz_a_ok;", "{text}");
}

/// The shape of the standard's example, unchanged: two macros nested in one another still work.
#[test]
fn a_macro_nested_in_its_own_argument_still_expands() {
    let text = rendered("#define E(x) x\nint E(E(1) + 2) = 0;\n");
    assert_eq!(text.trim(), "int 1 + 2 = 0;", "{text}");
}

/// And the rule's other half, which must keep working: `>` `>` has to stay separable, `::` `std` must stay joined.
#[test]
fn a_qualified_name_does_not_acquire_spaces() {
    let text = rendered("#define STD ::std::\nSTD vector<int> v;\n");
    assert!(
        text.contains("::std::") && !text.contains(":: std ::"),
        "{text}"
    );
}
/// **An identifier that begins with a keyword is one token.** The separator rule asks the lexer what a joined
/// spelling would be, and a lexer that answered `static` for `staticconstexpr` would say "no merge" and let the two
/// be written as one word — which is exactly the `staticconstexpr` this file exists for.
#[test]
fn an_identifier_that_starts_with_a_keyword_lexes_whole() {
    let (kind, range) =
        cpp_parser::lex_first("staticconstexpr", &cpp_parser::LexerConfig::default()).unwrap();
    assert_eq!(range.end_offset(), "staticconstexpr".len(), "{kind:?}");
}

/// **`#line N "name"` changes what the following lines report** — the standard's only way to change `__LINE__` and
/// `__FILE__`. The directive's own line keeps its physical number; the next line is `N`, and the name, when written,
/// replaces the file's from there on.
#[test]
fn a_line_directive_changes_the_line_and_the_file() {
    let source = "\
int a = __LINE__;
#line 100 \"x.c\"
int b = __LINE__;
const char *f = __FILE__;
int c = __LINE__;
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(
        source,
        &tokens,
        &nothing_defined,
        Some(std::path::Path::new("src\\a.cpp")),
    )
    .render()
    .text;
    assert!(
        text.contains("int a = 1;"),
        "before the directive the physical line is the answer: {text}"
    );
    assert!(
        text.contains("int b = 100;"),
        "the line after `#line 100` is 100, not 3: {text}"
    );
    assert!(
        text.contains("\"x.c\""),
        "and the name replaces the file's from there on: {text}"
    );
    assert!(
        text.contains("int c = 102;"),
        "the line after it continues the presumed numbering: {text}"
    );
}

/// **`#line`'s arguments are macro-expanded** — a preprocessor reads `#line L "y.c"` with `L` expanded, and a
/// directive whose number is a macro was read as no directive at all before.
#[test]
fn a_line_directives_number_is_expanded() {
    let source = "\
#define L 50
#line L \"y.c\"
int a = __LINE__;
const char *f = __FILE__;
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(
        text.contains("int a = 50;"),
        "the line after `#line L` is 50: {text}"
    );
    assert!(text.contains("\"y.c\""), "and the name is `y.c`: {text}");
}

/// **`#if __LINE__` asks the directive's own line** — the place a condition used to take the undefined-name branch,
/// because the evaluator is handed a macro table and these two names are bound by no `#define`.
#[test]
fn a_condition_answers_the_line() {
    let source = "\
#if __LINE__ == 1
int one = 1;
#else
int not_one = 1;
#endif
#if __LINE__ > 1
int after = 1;
#endif
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(text.contains("int one = 1;"), "line 1: {text}");
    assert!(text.contains("int after = 1;"), "line 6 > 1: {text}");
    assert!(!text.contains("not_one"), "the else is dead: {text}");
}

/// **And so do `#ifdef` and `defined`** — the two are defined names, in a condition or beside one.
#[test]
fn a_condition_answers_the_definitions() {
    let source = "\
#ifdef __LINE__
int line_defined = 1;
#endif
#ifndef __FILE__
int file_missing = 1;
#endif
#if defined(__FILE__)
int file_defined = 1;
#endif
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(text.contains("int line_defined = 1;"), "{text}");
    assert!(text.contains("int file_defined = 1;"), "{text}");
    assert!(!text.contains("file_missing"), "{text}");
}

/// **A `#line` moves the line a condition answers**, because the condition asks the same state a macro body does.
#[test]
fn a_condition_answers_the_line_after_a_line_directive() {
    let source = "\
#line 100 \"x.c\"
#if __LINE__ == 100
int yes = 1;
#endif
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(text.contains("int yes = 1;"), "{text}");
}

/// **An `#elif` answers it too**, and the value is the `#elif`'s own line.
#[test]
fn an_elif_answers_the_line() {
    let source = "\
#if 0
int no = 1;
#elif __LINE__ == 3
int yes = 1;
#else
int also_no = 1;
#endif
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(
        text.contains("int yes = 1;"),
        "the elif's line is 3: {text}"
    );
    assert!(!text.contains("no = 1;"), "{text}");
    assert!(!text.contains("also_no"), "{text}");
}

/// **And through a macro body** — `#define L __LINE__` then `#if L == 2` answers the `#if`'s line, because the body
/// is expanded where the condition is asked, not where it was written.
#[test]
fn a_condition_answers_the_line_through_a_macro() {
    let source = "\
#define L __LINE__
#if L == 2
int yes = 1;
#endif
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(text.contains("int yes = 1;"), "{text}");
}

/// **A GNU line marker is a `#line`.** `# 100 "x.c" 1 2` is the shape a preprocessor writes; its number is not a
/// directive name.
#[test]
fn a_gnu_line_marker_changes_the_line_and_the_file() {
    let source = "\
int a = __LINE__;
# 100 \"x.c\" 1
int b = __LINE__;
const char *f = __FILE__;
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(source, &tokens, &nothing_defined, None)
        .render()
        .text;
    assert!(
        text.contains("int b = 100;"),
        "the line after the marker is 100: {text}"
    );
    assert!(text.contains("\"x.c\""), "and the name is `x.c`: {text}");
}

/// **`__FILE__` and `__LINE__` answer the use** — the file the caller named and the line the token is written on.
///
/// `__FILE__` used to be the empty literal everywhere, and a cook told no file keeps that answer (a fixture has no
/// name); `cook_with_file` is what the real paths pass. The line is the line of the **use**, which is line 4 here, not
/// the line of the `#define`.
#[test]
fn the_file_and_the_line_macros_answer_the_use() {
    let source = "\
#define A __LINE__
#define B __FILE__
int a = 1;
int b = A;
const char *f = B;
";
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    let text = cook_with_file(
        source,
        &tokens,
        &nothing_defined,
        Some(std::path::Path::new("src\\main.cpp")),
    )
    .render()
    .text;
    assert!(text.contains("int b =4;"), "`__LINE__` is line 4: {text}");
    assert!(
        text.contains(r#""src\\main.cpp""#),
        "`__FILE__` is the name the cook was given, escaped as a literal: {text}"
    );
}
