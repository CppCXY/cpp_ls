//! **Empty arguments in a function-like macro invocation** — the last error in the standard library's stitched stream.
//!
//! MSVC's `<xtr1common>` writes:
//!
//! ```cpp
//! _NON_MEMBER_CALL(_IS_MEMFUNPTR_EXPLICIT_THIS_GUIDES, , , )
//! _NON_MEMBER_CALL(_IS_MEMFUNPTR_EXPLICIT_THIS_GUIDES, , , noexcept)
//! ```
//!
//! — a macro called with **four arguments, three of them empty**. In the rendering of `import std;` those two lines are
//! still in the text (4.85 MB, 224 files, one error left in the whole stream), so the expander left the invocation
//! alone. A macro call that is not expanded is not a declaration: the parser reads `, , , noexcept)` and reports it.

use cpp_code_analysis::macros::MacroTable;
use cpp_code_analysis::preprocess::cooked::cook_with;

/// Cook `source` with no macros to begin with — the `#define`s in it are what the walk reads.
fn cooked(source: &str) -> String {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    cook_with(source, &tokens, &MacroTable::new()).render().text
}

/// **A function-like macro called with empty arguments.**
#[test]
fn an_invocation_with_empty_arguments_expands() {
    let source = "#define F(a, b, c, d) int a##b##c##d;\nF(X, , , )\n";
    let rendered = cooked(source);
    assert!(
        !rendered.contains("F("),
        "the invocation was expanded: {rendered:?}"
    );
    assert!(
        rendered.contains("int X"),
        "and its arguments were substituted: {rendered:?}"
    );
}

/// **The measured shape**: a variadic macro with an empty middle argument and a trailing keyword argument.
#[test]
fn a_variadic_invocation_with_an_empty_argument_expands() {
    let source = "#define NON_MEMBER_CALL(NAME, ...) struct NAME##_tag {};\n\
                  NON_MEMBER_CALL(_MEMFUNPTR, , , )\n\
                  NON_MEMBER_CALL(_MEMFUNPTR, , , noexcept)\n";
    let rendered = cooked(source);
    assert!(
        !rendered.contains("NON_MEMBER_CALL("),
        "both invocations were expanded: {rendered:?}"
    );
    assert!(
        rendered.matches("struct _MEMFUNPTR_tag").count() == 2,
        "twice, once per invocation: {rendered:?}"
    );
}

/// **And the token before a comma stays one argument**: `, , , noexcept` is four arguments, not one.
#[test]
fn empty_arguments_do_not_swallow_the_next_one() {
    let source = "#define SHOW(a, b, c) [a][b][c]\nSHOW(X, , noexcept)\n";
    let rendered = cooked(source);
    assert!(!rendered.contains("SHOW("), "expanded: {rendered:?}");
    assert!(
        rendered.contains("[noexcept]"),
        "the third argument is `noexcept`, not lost to the empty second: {rendered:?}"
    );
}
