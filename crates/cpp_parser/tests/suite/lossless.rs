//! **Every byte of the file appears exactly once in the tree** — invariant I1, which everything above the parser
//! relies on: a formatter rewrites text from the tree, a language server maps a client's position through it, and a
//! rename edits the ranges in it. A tree that drops bytes is not a wrong answer, it is a *shifted* one: every offset
//! after the dropped run points at the wrong place.
//!
//! The tests here are the invariant made checkable: a file that parses cleanly must give its text back byte for byte, a
//! file with a comment in it must keep the comment, and the container that reads declarations until a `}` must keep the
//! comments between them. The last one **was** a failure — `docs/parser-review.md` §4 has the measurement (22 315 bytes in,
//! 19 987 out, zero syntax errors) and the cause: the linkage block's loop did not emit the trivia a declaration's `;`
//! stops before, so the comment was at its cursor when the next iteration began.

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

/// **A comment between two declarations is kept — outside a linkage block, which is the shape that always worked.**
///
/// The control for the test below: the same text, without `extern "C" { … }`, round trips. That is what makes the failure
/// the linkage block's rather than the comment machinery's.
#[test]
fn a_comment_between_two_declarations_survives() {
    round_trip("// one\nint f(int a, int b);\n// two\nint g(int a, int b);\n");
}

/// **A comment inside `extern "C" { … }` is kept, and so is every byte after it.**
///
/// This was the parser's one correctness bug: the comment's bytes were consumed and emitted nowhere, the tree kept a
/// **zero-width `ErrorNode`** in its place, and every offset after it shifted. The mechanism was the container's, not the
/// comment machinery's:
///
/// ```text
///   parse_declaration     reads `int f(int a, int b);`; its `;` deliberately stops **before** the comment
///   the linkage loop      begins its next iteration with the cursor on that comment
///   parse_declaration     fails on a comment
///   the recovery branch   marks an `ErrorNode` and calls `bump`, which emits nothing for a trivia token
/// ```
///
/// The class body's loop already emitted the trivia at the top of every iteration (`parse_class_body_members`); the
/// linkage block's is the second container and did not. It does now, which is what this test pins: `cxxabi.h` gave 22 315
/// bytes in and 19 987 out before the fix, and the whole file after it.
#[test]
fn a_comment_between_two_declarations_in_a_linkage_block_survives() {
    round_trip("extern \"C\" {\n// one\nint f(int a, int b);\n// two\nint g(int a, int b);\n}\n");
    // The shapes around it: a comment before the closing brace, a block comment, a comment after a definition whose body
    // ends inside the block, and a declaration after the block.
    round_trip("extern \"C\" {\nint f(int a);\n// before the brace\n}\n");
    round_trip("extern \"C\" {\n/* one */ int f(int a);\n/* two */\nint g(int a);\n}\n");
    round_trip("extern \"C\" {\nint f(int a) { return a; } // trailing\nint g(int a);\n}\n");
    round_trip("extern \"C\" {\nint f(int a);\n// two\nint g(int a);\n}\nint after;\n");
}
