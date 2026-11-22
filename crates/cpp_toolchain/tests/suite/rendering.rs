//! **The rendering's separator rule** — layout a parser can read back.
//!
//! A rendering is built by spelling the cooked tokens out in order, separated by a space **unless** joining them would
//! change the first token (so `::` and `std` stay touching, `+` and `+` do not). The rule exists because the parser
//! reads `&str`: a `>` `>` pair has to stay joinable, and a qualified name must not acquire spaces inside it.
//!
//! The failure this file was written for: `static` followed by a macro that expands to `constexpr` rendered as
//! `staticconstexpr`, which no parser can read and no C++ compiler would accept.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

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
