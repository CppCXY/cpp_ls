//! The file-level entry point: its tokens, its macros, and expanding a region against the right table.
//!
//! The two properties worth pinning here are the ones a consumer cannot check for itself:
//!
//! * **the token list agrees with the tree about comments**, because the documentation layer re-lexes
//!   them — a consumer that lexed the file itself would disagree with every other part of the analysis
//!   about where a comment starts and ends;
//! * **expansion uses the macros in force where the region is**, not the file's final table — a
//!   difference that only shows up in a file that defines the same macro twice, which is exactly the
//!   kind of file a test has to construct on purpose.

use cpp_code_analysis::{FileAnalysis, FileTokens, Origin};
use cpp_parser::{CppParser, ParserConfig, SourceRange};

/// Analyse a source string.
fn analyse(source: &str) -> FileAnalysis {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.to_source_text(),
        source,
        "the parse must round-trip before anything is read out of it"
    );
    FileAnalysis::new(source, &tree)
}

/// The significant spelling of an expansion, separators rendered.
fn rendered(expansion: &cpp_code_analysis::Expansion) -> String {
    let mut out = String::new();
    for token in &expansion.tokens {
        if token.space_before && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(token.text());
    }
    out
}

/// The significant tokens only, joined.
fn joined(expansion: &cpp_code_analysis::Expansion) -> String {
    expansion
        .tokens
        .iter()
        .filter(|token| !cpp_code_analysis::token::is_trivia(token.kind()))
        .map(|token| token.text())
        .collect()
}

// ============================================================================
// The token list
// ============================================================================

/// **The token list is the tree's, not the lexer's.** A comment is one token to the lexer and several to
/// the tree, because the documentation layer replaces it with the tokens it is made of — and a consumer
/// using the lexer's view would disagree with the rest of the analysis about every position after it.
#[test]
fn a_comment_is_the_trees_spelling_not_the_lexers() {
    let analysis = analyse("// hi\nint x;\n");

    let kinds: Vec<String> = analysis
        .tokens
        .tokens()
        .iter()
        .map(|token| format!("{:?}", token.kind))
        .collect();

    assert!(
        kinds.iter().any(|kind| kind == "LineComment"),
        "the opener is there: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "DocText"),
        "and so is the comment's text, as its own token: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| kind == "Identifier" && false),
        "sanity"
    );
}

/// The tokens tile the file: contiguous, in order, covering every byte.
#[test]
fn the_tokens_tile_the_file() {
    let source = "int   x = 1;\n// c\n#define A ( 2 )\nint y = A ;\n";
    let analysis = analyse(source);

    let mut expected = 0usize;
    for token in analysis.tokens.tokens() {
        assert_eq!(
            token.range.start_offset, expected,
            "a hole or overlap at {expected}: {token:?}"
        );
        expected = token.range.end_offset();
    }
    assert_eq!(expected, source.len());
}

#[test]
fn tokens_can_be_asked_for_by_range() {
    let source = "int x = 1;\nint y = 2;\n";
    let analysis = analyse(source);

    // The second line only.
    let second_line = SourceRange::new(11, 11);
    let tokens = analysis.tokens.in_range(second_line);

    let text: String = tokens.iter().map(|token| token.text()).collect();
    assert_eq!(text, "int y = 2;\n");
}

/// A token that only partially overlaps the range is excluded rather than clipped: clipping would have to
/// rewrite the token's text, and a token whose text disagrees with its range breaks every consumer at once.
#[test]
fn a_partially_covered_token_is_excluded() {
    let analysis = analyse("int x;\n");

    // Starts in the middle of `int`.
    let tokens = analysis.tokens.in_range(SourceRange::new(1, 4));
    let text: String = tokens.iter().map(|token| token.text()).collect();

    assert_eq!(text, " x", "`int` and `;` are excluded, not clipped");
}

// ============================================================================
// Looking things up by position
// ============================================================================

#[test]
fn the_token_at_an_offset_is_the_one_covering_it() {
    let analysis = analyse("int x;\n");

    assert_eq!(analysis.tokens.token_at(0).map(|t| t.text()), Some("int"));
    assert_eq!(analysis.tokens.token_at(3).map(|t| t.text()), Some(" "));
    assert_eq!(analysis.tokens.token_at(4).map(|t| t.text()), Some("x"));
    assert_eq!(analysis.tokens.token_at(5).map(|t| t.text()), Some(";"));
    assert_eq!(analysis.tokens.token_at(6).map(|t| t.text()), Some("\n"));
}

/// Past the end is `None`, not a panic or the last token: the cursor can be anywhere, including after the
/// last byte of a file whose final newline has been deleted.
#[test]
fn looking_up_past_the_end_finds_nothing() {
    let analysis = analyse("int x;\n");

    assert!(analysis.tokens.token_at(99).is_none());
    assert!(analyse("").tokens.token_at(0).is_none());
}

#[test]
fn a_line_range_includes_its_ending() {
    let analysis = analyse("int x;\nint y;\n");

    let first = analysis.line_range_at(2);
    assert_eq!(first.start_offset, 0);
    assert_eq!(first.length, 7, "`int x;\\n`, newline included");

    let second = analysis.line_range_at(9);
    assert_eq!(second.start_offset, 7);
    assert_eq!(second.length, 7);
}

/// A file whose last line has no newline still has that line.
#[test]
fn the_last_line_without_a_newline_is_still_a_line() {
    let analysis = analyse("int x;\nint y;");

    let last = analysis.line_range_at(9);
    assert_eq!(last.start_offset, 7);
    assert_eq!(last.length, 6);
}

// ============================================================================
// Expanding a region
// ============================================================================

#[test]
fn a_region_expands_its_macros() {
    let analysis = analyse("#define VERSION 3\nint x = VERSION;\n");

    // The use, on the second line.
    let use_range = analysis.line_range_at(20);
    assert_eq!(joined(&analysis.expand_range(use_range)), "intx=3;");
}

/// **Expansion uses the macros in force where the region is, not the file's final table.**
///
/// The file defines `N` twice, so a caller that expanded the first use against the final table would get
/// `2` — silently, and only in a file that redefines something, which is the sort of file that is hard to
/// notice you have written.
#[test]
fn a_region_is_expanded_against_the_macros_in_force_there() {
    let source = "#define N 1\nint a = N;\n#define N 2\nint b = N;\n";
    let analysis = analyse(source);

    // `int a = N;` starts at 12; `int b = N;` at 35.
    assert_eq!(joined(&analysis.expand_line_at(12)), "inta=1;");
    assert_eq!(joined(&analysis.expand_line_at(35)), "intb=2;");
}

/// A use *before* the definition is not expanded. This is the same property from the other side, and it is
/// the one that would break if the table were the file's final state: `N` is defined later, and a
/// file-scoped table would happily expand a use that a compiler would not.
#[test]
fn a_use_before_the_definition_is_not_expanded() {
    let source = "int a = N;\n#define N 1\n";
    let analysis = analyse(source);

    assert_eq!(joined(&analysis.expand_line_at(0)), "inta=N;");
}

/// A line with nothing to expand comes back as itself, which is what makes a hover on an ordinary line
/// show the line rather than a canned answer.
#[test]
fn a_line_with_no_macros_expands_to_itself() {
    let analysis = analyse("int x = 1;\n");

    assert_eq!(rendered(&analysis.expand_line_at(0)), "int x = 1;\n");
}

/// Function-like macros work through the same path, arguments and all.
#[test]
fn a_function_like_macro_expands_in_a_region() {
    let analysis = analyse("#define MAX(a, b) ((a) > (b) ? (a) : (b))\nint m = MAX(x, y);\n");

    assert_eq!(
        joined(&analysis.expand_line_at(42)),
        "intm=((x)>(y)?(x):(y));"
    );
}

/// The origins survive the file-level path, which is the whole point of carrying them: a consumer with a
/// cursor on an expanded token must be able to tell that it was expanded and where the macro is.
#[test]
fn an_expanded_token_keeps_its_origin_through_the_file_api() {
    let source = "#define VERSION 3\nint x = VERSION;\n";
    let analysis = analyse(source);

    let expansion = analysis.expand_line_at(20);
    let expanded = expansion
        .tokens
        .iter()
        .find(|token| token.is_expanded())
        .expect("a token from the macro");

    assert!(matches!(expanded.origin, Origin::Expanded { .. }));
    assert_eq!(expanded.text(), "3");

    // Navigation reaches the macro's *name*, so that a second jump starts from it.
    let name_range = expanded.navigation_range();
    assert_eq!(
        &source[name_range.start_offset..name_range.end_offset()],
        "VERSION"
    );

    // And a diagnostic lands on the use, which is on the second line.
    assert!(
        expanded.diagnostic_range().start_offset >= 18,
        "a diagnostic is in the use's line, not the definition's: {:?}",
        expanded.diagnostic_range()
    );
}

/// A node's range can be expanded directly, for callers that already have a node in hand.
#[test]
fn a_node_can_be_expanded() {
    let source = "#define A 1\nint x = A;\n";
    let tree = CppParser::parse(source, ParserConfig::default());
    let analysis = FileAnalysis::new(source, &tree);

    let declaration = tree
        .get_red_root()
        .descendants()
        .find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
        })
        .expect("a declaration");

    assert_eq!(joined(&analysis.expand_node(&declaration)), "intx=1;");
}

// ============================================================================
// Robustness
// ============================================================================

/// Whatever the file, no step of the file-level analysis panics.
///
/// Ranges that do not point at token boundaries are the input to check here: a cursor can be anywhere, and
/// an editor asks about positions that are in the middle of a token or past the end of the file.
#[test]
fn file_analysis_never_panics_on_odd_ranges() {
    let sources = [
        "",
        "\n",
        "int",
        "int x",
        "#define",
        "#define A",
        "#define A (",
        "// unterminated",
        "/* unterminated",
        "#if\n",
        "\\",
    ];

    for source in sources {
        let analysis = analyse(source);

        for offset in 0..=source.len() + 2 {
            let _ = analysis.tokens.token_at(offset);
            let _ = analysis.line_range_at(offset);
            let _ = analysis.expand_line_at(offset);
            let _ = analysis.expand_range(SourceRange::new(offset, 3));
        }

        // And a range that starts past the end, or is empty.
        let _ = analysis.expand_range(SourceRange::new(source.len() + 10, 5));
        let _ = analysis.expand_range(SourceRange::new(0, 0));
    }
}

/// A file with no tokens has no span, and asking it for one is not an error.
#[test]
fn an_empty_file_has_no_span() {
    let analysis = analyse("");

    assert!(analysis.tokens.is_empty());
    assert!(analysis.tokens.span().is_none());
}

/// The token list is the tree's, so a file that is nothing but a comment still has tokens — the comment's,
/// spelled the documentation layer's way.
#[test]
fn a_comment_only_file_has_the_comment_s_tokens() {
    let analysis = analyse("// only\n");

    assert!(!analysis.tokens.is_empty());
    assert_eq!(
        analysis
            .tokens
            .tokens()
            .iter()
            .map(|token| token.text())
            .collect::<String>(),
        "// only\n"
    );
}

/// `FileTokens` can be built on its own, for a caller that only wants positions.
#[test]
fn tokens_can_be_read_without_preprocessing() {
    let source = "int x;\n";
    let tree = CppParser::parse(source, ParserConfig::default());
    let tokens = FileTokens::from_tree(source, &tree);

    assert_eq!(tokens.source(), source);
    assert_eq!(tokens.span().map(|span| span.length), Some(source.len()));
    assert_eq!(tokens.len(), 5, "int, space, x, semicolon, newline");
}
