//! **Every byte of the file appears exactly once in the tree** — invariant I1, which everything above the parser
//! relies on: a formatter rewrites text from the tree, a language server maps a client's position through it, and a
//! rename edits the ranges in it. A tree that drops bytes is not a wrong answer, it is a *shifted* one: every offset
//! after the dropped run points at the wrong place.
//!
//! The two tests here are the invariant made checkable: a file that parses cleanly must give its text back byte for
//! byte, and a file with a comment in it must keep the comment. The second one fails today — see
//! [`a_comment_between_two_declarations_in_a_linkage_block_survives`] — and `docs/parser-review.md` §4 says where.

use cpp_parser::{CppParser, ParserConfig};

/// Parse and give the text back, asserting the round trip.
fn round_trip(source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.to_source_text(),
        source,
        "the tree must hold every byte of the file"
    );
}

#[test]
fn a_file_with_comments_round_trips() {
    round_trip("// a leading comment\nint f(int a);\n// between two declarations\nint g(int b);\n");
    round_trip("int f(int a) { return a; } // trailing\n");
    round_trip("/* block */ int x = 1; /* another */\n");
}

/// **A comment between two declarations is kept — outside a linkage block, which is the shape that works.**
///
/// This is the control for the test below: the same text, without `extern "C" { … }`, round trips. That is what makes
/// the failure the linkage block's rather than the comment machinery's.
#[test]
fn a_comment_between_two_declarations_survives() {
    round_trip("// one\nint f(int a, int b);\n// two\nint g(int a, int b);\n");
}

/// **A comment inside `extern "C" { … }` is dropped, and with it every byte after it.**
///
/// Measured: this 6-line file loses the second comment's bytes, and the tree keeps only the first one. The tree's own
/// dump shows the mechanism — an **empty `ErrorNode`** where the comment was:
///
/// ```text
/// Syntax(CompoundStat)
///   Syntax(DocComment)          ← the first one survived
///   Syntax(Declaration)         ← f0
///   Token(Newline), Token(Whitespace)
///   Syntax(ErrorNode)           ← zero-width: the comment's bytes were consumed and emitted nowhere
///   Syntax(Declaration)         ← f1
/// ```
///
/// `parse_linkage_block`'s recovery branch marks an `ErrorNode` and calls `bump`, and `bump` emits nothing when the
/// token at the cursor is **trivia** — comments are trivia to `CppParser::skip_trivia`, which is what makes the node
/// empty and the comment's text vanish. `bump` now asserts that this cannot happen, and in a debug build this test
/// fails on **that** assertion rather than on the round trip:
///
/// ```text
/// the cursor is on LineComment at 23, …    at decls.rs:5786
/// ```
///
/// A trace of the loop shows where it came from — `cursor IntKeyword at 8`, then `cursor LineComment at 23` on the
/// next iteration — so `parse_declaration` leaves the cursor on the trailing comment it just passed, and the loop
/// meets a token it cannot advance past. That is where the fix belongs, not in the loop.
///
/// `extern "C"` is not an exotic shape: it is in every C API header, and `cxxabi.h` is where this was first measured
/// (22 315 bytes in, 19 987 bytes out — 677 lines of 717).
#[ignore = "a comment inside a linkage block is consumed and never emitted — docs/parser-review.md §4"]
#[test]
fn a_comment_between_two_declarations_in_a_linkage_block_survives() {
    round_trip("extern \"C\" {\n// one\nint f(int a, int b);\n// two\nint g(int a, int b);\n}\n");
}
