//! Lexer behaviour: the C++ tokenisation rules that are easy to get wrong, and that a context-free
//! parser cannot recover from.
//!
//! The tests here are deliberately about *whole literals*. A lexer that cuts a raw string short
//! does not merely produce one wrong token — it re-lexes the tail of the raw string as C++, which
//! corrupts everything downstream. So each case asserts the exact token text, not just the kind.

use cpp_parser::{CppLanguageLevel, CppLexer, CppTokenKind, LexerConfig};

/// `(kind, text)` for every token, trivia included. Keeping trivia in makes the expectations
/// explicit and doubles as a losslessness check.
fn tokens(source: &str) -> Vec<(CppTokenKind, String)> {
    let config = LexerConfig::new(CppLanguageLevel::Cpp23);
    let mut errors = Vec::new();
    let lexed = CppLexer::new(source, config, &mut errors).tokenize();

    lexed
        .iter()
        .map(|token| {
            (
                token.kind,
                source[token.range.start_offset..token.range.end_offset()].to_string(),
            )
        })
        .collect()
}

/// Like [`tokens`], but with `$` accepted in identifiers.
fn tokens_with_dollar(source: &str) -> Vec<(CppTokenKind, String)> {
    let config = LexerConfig::new(CppLanguageLevel::Cpp23).with_dollar_in_identifier(true);
    let mut errors = Vec::new();
    let lexed = CppLexer::new(source, config, &mut errors).tokenize();

    lexed
        .iter()
        .map(|token| {
            (
                token.kind,
                source[token.range.start_offset..token.range.end_offset()].to_string(),
            )
        })
        .collect()
}

fn errors_for(source: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let _ = CppLexer::new(source, LexerConfig::default(), &mut errors).tokenize();
    errors.into_iter().map(|error| error.message).collect()
}

/// Non-trivia tokens only, as `(kind, text)`.
fn significant(source: &str) -> Vec<(CppTokenKind, String)> {
    tokens(source)
        .into_iter()
        .filter(|(kind, _)| {
            !matches!(
                kind,
                CppTokenKind::Whitespace | CppTokenKind::Newline | CppTokenKind::LineContinuation
            )
        })
        .collect()
}

/// Every token must round-trip back to the input, byte for byte.
#[track_caller]
fn assert_lossless(source: &str) {
    let rebuilt: String = tokens(source).into_iter().map(|(_, text)| text).collect();
    assert_eq!(rebuilt, source, "token texts do not reconstruct the input");
}

#[test]
fn raw_string_literals_are_one_token() {
    // The whole point: a raw string containing quotes and backslashes must not break the token.
    for source in [
        r#"R"(simple)"#,
        r#"R"(has "quotes" inside)"#,
        r#"R"(backslash \n stays literal)"#,
        r#"R"delim(contains )" without ending)delim""#,
        r#"R"x(a)b"x""#,
        r#"R"()""#,
    ] {
        let lexed = significant(source);
        assert_eq!(
            lexed,
            vec![(CppTokenKind::StringLiteral, source.to_string())],
            "raw string {source:?} was not lexed as a single literal"
        );
        assert_lossless(source);
    }
}

#[test]
fn raw_strings_with_encoding_prefixes() {
    for source in [
        r#"u8R"(bytes)""#,
        r#"uR"(utf16)""#,
        r#"UR"(utf32)""#,
        r#"LR"(wide)""#,
    ] {
        let lexed = significant(source);
        assert_eq!(
            lexed,
            vec![(CppTokenKind::StringLiteral, source.to_string())],
            "{source:?} was not lexed as a single literal"
        );
        assert_lossless(source);
    }
}

#[test]
fn raw_string_stops_at_its_own_delimiter() {
    // `)delim"` ends it; a bare `)"` inside must not.
    let source = r#"R"d(a )" b) d")d""#;
    let lexed = significant(source);
    assert_eq!(lexed[0].1, r#"R"d(a )" b) d")d""#);
    assert_eq!(lexed[0].0, CppTokenKind::StringLiteral);
}

#[test]
fn prefixed_literals_are_single_tokens() {
    for (source, kind) in [
        ("u8\"text\"", CppTokenKind::StringLiteral),
        ("u\"text\"", CppTokenKind::StringLiteral),
        ("U\"text\"", CppTokenKind::StringLiteral),
        ("L\"text\"", CppTokenKind::StringLiteral),
        ("u8'c'", CppTokenKind::CharLiteral),
        ("u'c'", CppTokenKind::CharLiteral),
        ("U'c'", CppTokenKind::CharLiteral),
        ("L'c'", CppTokenKind::CharLiteral),
        ("'c'", CppTokenKind::CharLiteral),
        ("\"text\"", CppTokenKind::StringLiteral),
    ] {
        let lexed = significant(source);
        assert_eq!(
            lexed,
            vec![(kind, source.to_string())],
            "{source:?} was not lexed as one literal"
        );
    }
}

#[test]
fn lone_prefix_letters_are_identifiers() {
    // The prefixes are only prefixes when a literal follows. `u8x` is a name, `R` is a name,
    // `u + 1` is an addition.
    for (source, expected) in [
        ("u8x", vec!["u8x"]),
        ("u", vec!["u"]),
        ("L", vec!["L"]),
        ("R", vec!["R"]),
        ("uuid", vec!["uuid"]),
        ("Lvalue", vec!["Lvalue"]),
    ] {
        let lexed = significant(source);
        assert_eq!(
            lexed,
            vec![(CppTokenKind::Identifier, expected[0].to_string())],
            "{source:?} must be a plain identifier"
        );
    }
}

#[test]
fn raw_strings_are_reported_before_cpp11() {
    let mut errors = Vec::new();
    let config = LexerConfig::new(CppLanguageLevel::Cpp11).with_string_prefixes(false);
    let lexed = CppLexer::new(r#"R"(x)""#, config, &mut errors).tokenize();

    // Without raw-string support the `R` is a name and the string is empty, which is the honest
    // reading of the source under those rules.
    assert_eq!(lexed[0].kind, CppTokenKind::Identifier);
    assert_eq!(lexed[1].kind, CppTokenKind::StringLiteral);
}

#[test]
fn digit_separators_are_part_of_the_number() {
    for source in ["1'000'000", "0xFF'FF", "0b1010'1010", "1.5'000", "1e1'0"] {
        let lexed = significant(source);
        assert_eq!(
            lexed,
            vec![(
                if source.contains('.') || source.contains('e') {
                    CppTokenKind::FloatingLiteral
                } else {
                    CppTokenKind::IntegerLiteral
                },
                source.to_string()
            )],
            "{source:?} was not lexed as one number"
        );
    }
}

#[test]
fn digit_separator_requires_a_digit_after_it() {
    // `1'` is the integer `1` followed by a user-defined suffix marker, not the number `1'`.
    let lexed = significant("1'");
    assert_eq!(
        lexed.len(),
        2,
        "expected the number and the quote, got {lexed:?}"
    );
    assert_eq!(lexed[0], (CppTokenKind::IntegerLiteral, "1".to_string()));
}

#[test]
fn user_defined_literal_suffixes_get_their_own_kind() {
    for source in ["42_km", "3.14_deg", "\"hello\"_s", "'c'_x"] {
        let lexed = significant(source);
        assert_eq!(
            lexed.last().map(|(kind, _)| *kind),
            Some(CppTokenKind::UserDefinedLiteral),
            "{source:?} did not produce a user-defined literal"
        );
    }
}

#[test]
fn standard_number_suffixes_stay_integer_or_float() {
    for (source, kind) in [
        ("42u", CppTokenKind::IntegerLiteral),
        ("42ULL", CppTokenKind::IntegerLiteral),
        ("42ll", CppTokenKind::IntegerLiteral),
        ("42z", CppTokenKind::IntegerLiteral),
        ("1.0f", CppTokenKind::FloatingLiteral),
        ("1.0L", CppTokenKind::FloatingLiteral),
        ("0x1p3", CppTokenKind::FloatingLiteral),
    ] {
        let lexed = significant(source);
        assert_eq!(lexed, vec![(kind, source.to_string())], "{source:?}");
    }
}

#[test]
fn line_splices_are_their_own_token() {
    for source in ["a\\\nb", "a\\\r\nb", "a\\\r b"] {
        let lexed = tokens(source);
        assert!(
            lexed
                .iter()
                .any(|(kind, _)| *kind == CppTokenKind::LineContinuation),
            "{source:?} produced no line-continuation token: {lexed:?}"
        );
        assert_lossless(source);
    }
}

#[test]
fn line_splices_do_not_split_directives_or_identifiers() {
    // `#define GREETING \<newline> "hi"` is one directive spelled across two lines.
    let source = "#define GREETING \\\n  \"hi\"\n";
    let kinds: Vec<CppTokenKind> = tokens(source).into_iter().map(|(kind, _)| kind).collect();

    assert!(kinds.contains(&CppTokenKind::LineContinuation));
    assert!(kinds.contains(&CppTokenKind::StringLiteral));
    assert_lossless(source);
}

#[test]
fn stray_backslash_is_reported() {
    let errors = errors_for("int x = 1 \\ 2;");
    assert!(
        errors.iter().any(|error| error.contains("stray")),
        "expected a stray-backslash diagnostic, got {errors:?}"
    );
}

#[test]
fn block_comments_do_not_nest() {
    // `/* /* */` ends at the first `*/`, so the code after it is real code.
    let source = "/* /* */ int x;";
    let lexed = significant(source);
    assert_eq!(
        lexed,
        vec![
            (CppTokenKind::BlockComment, "/* /* */".to_string()),
            (CppTokenKind::IntKeyword, "int".to_string()),
            (CppTokenKind::Identifier, "x".to_string()),
            (CppTokenKind::Semicolon, ";".to_string()),
        ]
    );
    assert!(
        errors_for(source).is_empty(),
        "valid nested-looking comment must not be reported"
    );
}

#[test]
fn unicode_identifiers_are_accepted() {
    for source in ["int café;", "int λ;", "int 中文;", "int Ж;"] {
        let lexed = significant(source);
        assert_eq!(lexed[0].0, CppTokenKind::IntKeyword, "{source:?}");
        assert_eq!(
            lexed[1],
            (
                CppTokenKind::Identifier,
                source
                    .trim_start_matches("int ")
                    .trim_end_matches(';')
                    .to_string()
            ),
            "{source:?} did not lex its Unicode identifier"
        );
    }
}

#[test]
fn universal_character_names_in_identifiers() {
    let lexed = significant("int \\u00e9;");
    assert_eq!(
        lexed[1],
        (CppTokenKind::Identifier, "\\u00e9".to_string()),
        "a universal character name must stay part of the identifier"
    );
}

#[test]
fn emoji_is_not_an_identifier() {
    let errors = errors_for("int 😀;");
    assert!(
        errors.iter().any(|error| error.contains("unrecognized")),
        "an emoji is not XID and must be reported, got {errors:?}"
    );
}

#[test]
fn dollar_is_gated_by_config() {
    assert_eq!(
        tokens_with_dollar("$foo")[0],
        (CppTokenKind::Identifier, "$foo".to_string())
    );
    assert_ne!(tokens("$foo")[0].0, CppTokenKind::Identifier);
}

#[test]
fn header_names_need_an_explicit_request() {
    // Outside a directive, `<iostream>` must stay three tokens or every template breaks.
    let lexed = significant("<iostream>");
    assert_eq!(
        lexed.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        vec![
            CppTokenKind::Less,
            CppTokenKind::Identifier,
            CppTokenKind::Greater
        ]
    );
}

#[test]
fn lex_header_name_reads_angles_and_quotes() {
    for source in [
        "<iostream>",
        "<sys/types.h>",
        "\"local.h\"",
        "\"./rel/path.h\"",
    ] {
        let mut errors = Vec::new();
        let mut lexer = CppLexer::new(source, LexerConfig::default(), &mut errors);

        let kind = lexer
            .lex_header_name()
            .unwrap_or_else(|| panic!("{source:?} was not recognised as a header name"));
        assert_eq!(kind, CppTokenKind::HeaderName);

        // Everything must have been consumed: a partial header name would leave the tail to be
        // lexed as C++.
        assert_eq!(lexer.tokenize().len(), 0, "{source:?} left tokens behind");
        assert!(errors.is_empty());
    }
}

#[test]
fn lex_header_name_declines_when_it_is_not_one() {
    // These must fall back to ordinary tokenisation without consuming anything.
    for source in ["<vector", "<", ">", "\"unterminated", "a < b", "<a;b>"] {
        let mut errors = Vec::new();
        let mut lexer = CppLexer::new(source, LexerConfig::default(), &mut errors);

        assert!(
            lexer.lex_header_name().is_none(),
            "{source:?} must not be taken for a header name"
        );

        // Nothing consumed, so the caller can lex normally.
        let rest = lexer.tokenize();
        let reconstructed: String = rest
            .iter()
            .map(|token| &source[token.range.start_offset..token.range.end_offset()])
            .collect();
        assert_eq!(reconstructed, source, "{source:?} was partially consumed");
    }
}

#[test]
fn token_stream_is_always_lossless() {
    let corpus = [
        "",
        " ",
        "int x;",
        r#"R"(a "b" c)"#,
        "a\\\nb",
        "/* /* */ x",
        "1'000'000",
        "42_km",
        "int café = 1;",
        "#define F \\\n 1\n",
        "\"\\\"\"",
        "'\\''",
    ];

    for source in corpus {
        assert_lossless(source);
    }
}

/// Numeric literals are scanned as one token, or not at all.
///
/// The cases here are the ones where the scanner has more than one reason to stop, so getting one
/// wrong splits a number in two and hands the tail to the parser as a *separate* literal. That
/// corruption is invisible in the token kinds — `0.0` came out as `0` then `.0`, both of which are
/// numbers — and it is what a `0.0` field initialiser in a class body tripped over.
#[test]
fn numeric_literals_are_single_tokens() {
    let cases = [
        "0.0",
        "0.",
        ".5",
        "1.5",
        "07",
        "0",
        "1e5",
        "1.0e-3",
        "0x1p3",
        "0x1f",
        "0b1010",
        "1'000'000",
        "42_km",
    ];

    for source in cases {
        let lexed = tokens(source);
        let significant: Vec<&(CppTokenKind, String)> = lexed
            .iter()
            .filter(|(kind, _)| !matches!(kind, CppTokenKind::Whitespace | CppTokenKind::Newline))
            .collect();

        assert_eq!(
            significant.len(),
            1,
            "{source:?} was split into {significant:?}"
        );
        assert_eq!(
            significant[0].1, source,
            "{source:?} was not scanned as a whole literal"
        );
    }
}

/// A **user-defined literal suffix is any identifier**, not only one that starts with `_`.
///
/// Requiring the underscore split everything else into two tokens — a number and a name — and the two families
/// that matters for are ordinary modern C++: the `std::chrono_literals` suffixes (`100ms`, `10s`, `2h`) and the
/// pasted-identifier arguments of a macro (`TEST(FormatPerformance, 1k_row)` in gtest, whose second argument is a
/// test name). A suffix that does not begin with `_` is *reserved*, which is a rule about programs rather than
/// about tokens: GCC and Clang lex one user-defined literal and warn.
#[test]
fn a_literal_suffix_need_not_start_with_an_underscore() {
    for source in [
        "100ms",
        "10s",
        "2h",
        "1k_row",
        "1_km",
        "\"name\"sv",
        "\"text\"_s",
        "'x'_c",
    ] {
        let lexed = significant(source);
        assert_eq!(lexed.len(), 1, "{source:?} was split into {lexed:?}");
        assert_eq!(
            lexed[0].0,
            CppTokenKind::UserDefinedLiteral,
            "{source:?} is one user-defined literal"
        );
        assert_eq!(lexed[0].1, source, "{source:?} is not cut short");
    }

    // What the language's own suffixes look like is unchanged, and that is the half worth pinning: an exponent, a
    // hex value, a digit separator and the standard suffixes (`u`, `LL`, `z`, `f`) are all consumed by the number
    // before the new rule is reached, so none of them becomes a user-defined literal.
    for source in [
        "1u",
        "1LL",
        "1z",
        "0x1f",
        "1e5",
        "1.5f",
        "1'000'000",
        "0b1010",
        "0x1p3",
    ] {
        let lexed = significant(source);
        assert_eq!(lexed.len(), 1, "{source:?} was split into {lexed:?}");
        assert_ne!(
            lexed[0].0,
            CppTokenKind::UserDefinedLiteral,
            "{source:?} has no user-defined suffix"
        );
    }

    // And a name that merely *follows* a number still does, because nothing separates them: `12abc` is one token
    // in every reading of C++, and it is ill-formed either way.
    assert_eq!(significant("12abc").len(), 1);
}

/// A **byte-order mark** is whitespace, not an unrecognised character.
///
/// `\u{feff}` at offset 0 is what Visual Studio writes when it saves a file as "UTF-8 with signature", and the
/// standard drops it in translation phase 1 — so a file saved that way is a perfectly good translation unit.
/// Reporting it as an unrecognised character put one diagnostic at **offset 0** of every such file, which is the
/// worst place a diagnostic can land: 37 of the 200 files in the first real C++ project this parser was pointed
/// at began that way. The same code point in the middle of a file is the zero-width no-break space, and is trivia
/// for the same reason.
#[test]
fn a_byte_order_mark_is_whitespace() {
    let source = "\u{feff}#pragma once\n";
    let errors = errors_for(source);
    assert!(errors.is_empty(), "a BOM is not an error: {errors:?}");

    // Trivia, and **one** token of it: the mark is not a character the lexer silently dropped on the floor, which
    // is what keeps the tree lossless.
    let lexed = tokens(source);
    assert_eq!(lexed[0].0, CppTokenKind::Whitespace);
    assert_eq!(lexed[0].1, "\u{feff}");

    // The token after it is the directive's `#`, and nothing was lost on the way.
    assert_eq!(
        significant(source),
        vec![
            (CppTokenKind::Hash, "#".to_string()),
            (CppTokenKind::Identifier, "pragma".to_string()),
            (CppTokenKind::Identifier, "once".to_string()),
        ]
    );

    // The same code point mid-file, where it is only ever a stray mark.
    assert!(errors_for("int x\u{feff}= 1;").is_empty());
}
