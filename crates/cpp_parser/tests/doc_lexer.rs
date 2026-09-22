//! The documentation-comment lexer.
//!
//! Two properties matter more than the individual token kinds, and both are asserted over a corpus
//! rather than case by case:
//!
//! * **Losslessness.** The tokens of a comment tile its text exactly. This is not decoration: once
//!   the C++ lexer's single comment token is replaced by doc tokens, these *are* the comment's text
//!   as far as the tree is concerned, so a byte lost here is a byte lost from the CST.
//! * **Coordinates.** Every range is in the original file, not an offset into the comment. The doc
//!   events are appended to the C++ event stream, and the tree builder slices the file with them.

use cpp_parser::{
    DocCommentStyle, DocToken, DocTokenKind, is_block_comment, is_documentation_comment, lex_comment,
};

fn lex(source: &str) -> (Vec<DocToken>, String) {
    let style = if is_block_comment(source) {
        DocCommentStyle::Block
    } else {
        DocCommentStyle::Line
    };
    let _ = style;
    let tokens = lex_comment(source, 0);
    let text: String = tokens
        .iter()
        .map(|token| {
            &source[token.range.start_offset..token.range.end_offset()]
        })
        .collect();
    (tokens, text)
}

/// `(kind, text)` pairs, which is what makes a failure readable.
fn pairs(source: &str) -> Vec<(DocTokenKind, String)> {
    lex(source)
        .0
        .iter()
        .map(|token| {
            (
                token.kind,
                source[token.range.start_offset..token.range.end_offset()].to_string(),
            )
        })
        .collect()
}

const CORPUS: &[&str] = &[
    "// plain",
    "/// doc",
    "//! doc",
    "//// banner, not a doc comment",
    "//",
    "///",
    "/** block doc */",
    "/*! block doc */",
    "/* plain block */",
    "/**/",
    "/* */",
    "/**\n * @brief x\n */",
    "/**\n * @param[in] a  the a\n * @param[out] b the b\n */",
    "/** @brief one line */",
    "/// @brief x\n/// @param y desc",
    "/// email user@example.com here",
    "/// a \\ b and 1 < 2 and a::b",
    "/// @code\n/// int x = 1; // @notacommand\n/// @endcode",
    "/// @ref Foo<T>",
    "/// @param x : described",
    "/* unterminated",
    "/**\n * **bold** and *em*\n */",
    "/// @param[1,3] range",
    "/// trailing @",
    "/// backslash \\param x",
];

#[test]
fn tokens_tile_the_comment_exactly() {
    for source in CORPUS {
        let (tokens, reconstructed) = lex(source);
        assert_eq!(
            reconstructed, *source,
            "{source:?} was not tiled by its tokens: {tokens:?}"
        );
    }
}

/// Losslessness has to hold when the comment starts somewhere other than offset zero, or the ranges
/// would silently be comment-relative and every node in the tree would point at the wrong text.
#[test]
fn ranges_are_in_file_coordinates() {
    let prefix = "int x;  ";
    let comment = "/// hi";
    let source = format!("{prefix}{comment}");

    let tokens = lex_comment(comment, prefix.len());

    assert!(!tokens.is_empty());
    assert_eq!(tokens[0].range.start_offset, prefix.len());
    assert_eq!(
        &source[tokens[0].range.start_offset..tokens[0].range.end_offset()],
        "///"
    );

    let last = tokens.last().unwrap();
    assert_eq!(
        last.range.end_offset(),
        prefix.len() + comment.len(),
        "the last token must end at the end of the comment, not at the end of the file"
    );
}

#[test]
fn the_third_character_decides_documentation() {
    let documented = [
        "/// x",
        "//! x",
        "/** x */",
        "/*! x */",
        "/*** x */",
    ];
    let not_documented = ["// x", "//// x", "/* x */", "/*!*/", "/**/", "", "x"];
    for source in documented {
        assert!(
            is_documentation_comment(source),
            "{source:?} should be documentation"
        );
    }
    for source in not_documented {
        assert!(
            !is_documentation_comment(source),
            "{source:?} should not be documentation"
        );
    }
}

#[test]
fn commands_are_recognised_and_split_from_their_introducer() {
    let lexed = pairs("/// @param x");

    let name = lexed
        .iter()
        .find(|(kind, _)| *kind == DocTokenKind::DocCommandName)
        .expect("a command name");

    assert_eq!(name.1, "param", "the name is the name, without the `@`");
    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocIntroducer && text == "@"),
        "the introducer is its own token: {lexed:?}"
    );
}

/// `\\param` is the same command as `@param`: Doxygen accepts both, and a file that uses the
/// backslash form must not be read as prose.
#[test]
fn backslash_introduces_commands_too() {
    let lexed = pairs("/// \\param x");

    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocCommandName && text == "param"),
        "{lexed:?}"
    );
}

/// An `@` with no name after it is prose. Treating it as an empty command would make
/// `user@example.com` lex as a command named `example`, and would give the grammar a command it can
/// never match.
#[test]
fn a_bare_at_sign_is_prose() {
    let lexed = pairs("/// mail user@example.com now");

    assert!(
        !lexed
            .iter()
            .any(|(kind, _)| *kind == DocTokenKind::DocCommandName),
        "no command should be found in prose: {lexed:?}"
    );
    assert!(
        lexed
            .iter()
            .any(|(_, text)| text.contains("user@example.com")),
        "the address should stay one run of text: {lexed:?}"
    );
}

/// A block comment's continuation `*` is layout, and stripping it is what lets the grammar see
/// `@param` at the start of a line rather than `* @param`.
#[test]
fn block_comment_continuation_markers_are_layout() {
    let source = "/**\n * @brief x\n */";
    let lexed = pairs(source);

    let name = lexed
        .iter()
        .find(|(kind, _)| *kind == DocTokenKind::DocCommandName)
        .expect("a command name");

    assert_eq!(name.1, "brief");
    assert!(
        !lexed.iter().any(|(_, text)| text == "*"),
        "the continuation marker should have been absorbed into whitespace: {lexed:?}"
    );
    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::BlockCommentEnd && text == "*/"),
        "the closer is a token of its own: {lexed:?}"
    );
}

/// The opening `/**` is not a continuation line, so its `*` must stay in the introducer.
#[test]
fn the_opening_line_is_not_a_continuation() {
    let lexed = pairs("/** @brief x */");

    assert_eq!(
        lexed.first().map(|(kind, _)| *kind),
        Some(DocTokenKind::DocBlockStart)
    );
    assert!(
        !lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocWhitespace && text.contains('*')),
        "{lexed:?}"
    );
}

/// `**bold**` at the start of a continuation line keeps its asterisks: a `*` followed by another `*`
/// is emphasis, not a line marker.
#[test]
fn emphasis_is_not_mistaken_for_a_line_marker() {
    let lexed = pairs("/**\n * **bold**\n */");

    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocText && text.contains("**bold**")),
        "{lexed:?}"
    );
}

/// An unterminated block comment is the normal state of a file being edited, and it must still tile.
#[test]
fn an_unterminated_block_comment_still_tiles() {
    let source = "/**\n * @brief x";
    let (_, reconstructed) = lex(source);

    assert_eq!(reconstructed, source);
}

/// Punctuation is only structural where the grammar reads it as structure. After `@param` the `[`
/// opens an argument list and the `]` closes it, so those are tokens; the same brackets in prose are
/// just characters.
///
/// The asymmetry is the point: breaking prose on punctuation would cut `std::vector<int>` into six
/// tokens and `user@example.com` into three, and the grammar would then have to reassemble them.
#[test]
fn punctuation_is_structural_only_after_a_command() {
    let lexed = pairs("/// @param[in] x");

    let punctuation: Vec<&str> = lexed
        .iter()
        .filter(|(kind, _)| {
            matches!(
                kind,
                DocTokenKind::DocLeftBracket | DocTokenKind::DocRightBracket
            )
        })
        .map(|(_, text)| text.as_str())
        .collect();

    assert_eq!(punctuation, vec!["[", "]"], "{lexed:?}");
}

/// Prose keeps its punctuation.
#[test]
fn prose_punctuation_stays_in_the_text() {
    let cases = [
        ("/// see std::vector<int> for details", "std::vector<int>"),
        ("/// mail user@example.com now", "user@example.com"),
        ("/// 1 < 2 and 3 > 2", "1 < 2 and 3 > 2"),
    ];

    for (source, expected) in cases {
        let lexed = pairs(source);
        assert!(
            lexed
                .iter()
                .any(|(kind, text)| *kind == DocTokenKind::DocText && text.contains(expected)),
            "{source:?} should keep {expected:?} as text: {lexed:?}"
        );
    }
}

/// Inside `@code`, `//` and `@` are code, not comments and commands. The grammar decides what to do
/// with that; the lexer's job is only to not lose the bytes.
#[test]
fn a_code_block_keeps_its_content() {
    let source = "/// @code\n/// int x = 1; // @notacommand\n/// @endcode";
    let (_, reconstructed) = lex(source);

    assert_eq!(reconstructed, source);
}

/// The space before a block comment's closer is its own token, not the last character of the text.
///
/// The lexer decides this because it is the only layer that can: the parser sees tokens, and by the
/// time it sees `"B "` as one text run the space is already indistinguishable from a space the author
/// meant. Leaving it in makes the brief `B `, which a consumer rendering the brief then prints.
#[test]
fn the_space_before_a_block_closer_is_not_text() {
    let lexed = pairs("/** @brief B */");

    let text = lexed
        .iter()
        .find(|(kind, _)| *kind == DocTokenKind::DocText)
        .expect("the brief's text");
    assert_eq!(text.1, "B", "the text run stops before the closer: {lexed:?}");

    // And the space really is still there — as layout, not as lost bytes.
    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocWhitespace && text == " "),
        "the space is emitted as layout: {lexed:?}"
    );
}

/// A single space inside a line does *not* end a text run: `@param const T& x` is one argument.
///
/// The two rules pull in opposite directions and are distinguished by what follows the spaces. Inside
/// a line the space is part of what was written; at the end of the comment's content it is not.
#[test]
fn a_space_inside_a_line_stays_in_the_text() {
    let lexed = pairs("/// @param const T& x");

    assert!(
        lexed
            .iter()
            .any(|(kind, text)| *kind == DocTokenKind::DocText && text == "const T& x"),
        "an inner space is text, not a separator: {lexed:?}"
    );
}

/// Whatever the input, lexing terminates and tiles. A comment is attacker-controlled text from the
/// editor's point of view — it is whatever the user has typed so far — so "does not spin" belongs in
/// a test rather than in an argument.
#[test]
fn lexing_always_terminates_and_tiles() {
    let cases = [
        "@",
        "\\",
        "////",
        "/***/",
        "/**/",
        "/*/",
        "/// @",
        "/// \\",
        "/// @param[",
        "/// ])(",
        "@@@@",
        "\\\\\\\\",
        "/// \u{1F600} emoji",
        "/** \u{4e2d}\u{6587} */",
        "/** x */",
        "/** x  */",
        "/** x\n */",
        "/**\n * x */",
        "/** */",
        "/**  ",
    ];

    for source in cases {
        let (tokens, reconstructed) = lex(source);
        assert_eq!(
            reconstructed, source,
            "{source:?} was not tiled: {tokens:?}"
        );
    }
}
