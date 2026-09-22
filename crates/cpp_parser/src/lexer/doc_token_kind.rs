//! Token kinds for the *documentation* layer.
//!
//! The C++ lexer hands a comment over as one opaque token: `// x` or `/** ... */`. Making sense of
//! what is inside is a second lexical pass over that text, and this is its alphabet.
//!
//! # Why a separate lexer rather than more `CppTokenKind` variants
//!
//! A comment's text is not C++. `@param[in] x` contains brackets and an at-sign that mean nothing in
//! C++, and `a * b` inside a comment is three words rather than a multiplication. Lexing it with the
//! C++ rules and then re-interpreting would mean the doc grammar had to undo decisions the C++ lexer
//! made for a language that is not there.
//!
//! # Two kinds describe the *same* text
//!
//! [`DocTokenKind::to_cpp_token`] maps every kind to the `CppTokenKind` it is written as. That is
//! what lets one event stream carry both layers: the doc parser emits tokens in C++ coordinates, and
//! the tree builder only ever sees the `CppTokenKind` half.
//!
//! The mapping is many-to-one for the structural tokens (`DocTokenKind::DocLeftBracket` and
//! `Doctor::DocRightBracket` both become `CppTokenKind::DocTrivia`), which is deliberate: their
//! *shape* is what the doc grammar dispatches on, while the tree only needs to know they are part of
//! a comment.

use crate::kind::CppTokenKind;

/// A token within a comment.
///
/// The set mirrors the C++ token kinds where the text is the same and adds only what Doxygen needs,
/// so that a reader who knows `CppTokenKind` can guess most of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum DocTokenKind {
    /// No token / initialisation, mirroring [`CppTokenKind::None`].
    None,

    // ========== The comment's own delimiters ==========
    /// The `///` or `//!` that opens a documentation line comment.
    DocLineStart,
    /// The `//` of an ordinary line comment.
    LineCommentStart,
    /// The `/**` or `/*!` that opens a documentation block comment.
    DocBlockStart,
    /// The `/*` of an ordinary block comment.
    BlockCommentStart,
    /// The `*/` that closes a block comment.
    BlockCommentEnd,

    // ========== Content ==========
    /// The `@` or `\` that introduces a command, without the name that follows it.
    ///
    /// Separate from its name so the name token's range is exactly the name — that is what a consumer
    /// matches against a command table, and a range that included the introducer would need trimming
    /// at every use.
    DocIntroducer,
    /// A command name without its introducer: `param`, `brief`, `returns`.
    DocCommandName,
    /// The running text of a description, up to the end of its line.
    DocText,
    /// Whitespace inside a comment. Kept as a token because a comment is part of the CST and must
    /// stay lossless, while the *doc* grammar skips it.
    DocWhitespace,
    /// A newline inside a comment. Ends whatever line-oriented construct is being read.
    DocNewline,

    // ========== Punctuation the doc grammar dispatches on ==========
    /// `[` — opens a direction specifier (`@param[in]`) or a range (`@param[1,3]`).
    DocLeftBracket,
    /// `]` — closes one.
    DocRightBracket,
    /// `,` — separates a range (`@param[1,3]`).
    DocComma,
    /// `(` — opens a reference or a `@defgroup`-style payload.
    DocLeftParen,
    /// `)` — closes one.
    DocRightParen,
    /// `.` — classifies a command (`@param.in`) and appears inside `@defgroup a.b`.
    DocDot,
    /// `:` — appears in `@param a : description` and in `@page` names.
    DocColon,
    /// `<` — opens a template-argument list in a reference (`@ref Foo<T>`).
    DocLess,
    /// `>` — closes one.
    DocGreater,
    /// `=` — appears in `@param x = default`.
    DocEquals,
    /// `::` — a scope resolution inside a reference.
    DocScope,

    /// The end of the comment's text.
    Eof,
}

impl DocTokenKind {
    /// The `CppTokenKind` this text is recorded as in the tree.
    ///
    /// Structural punctuation inside a comment is still *trivia* as far as the C++ grammar is
    /// concerned — it must never be mistaken for a bracket that closes a C++ construct — so most of
    /// this table maps onto the comment trivia kinds.
    pub fn to_cpp_token(self) -> CppTokenKind {
        match self {
            DocTokenKind::None => CppTokenKind::None,
            DocTokenKind::DocLineStart | DocTokenKind::LineCommentStart => CppTokenKind::LineComment,
            DocTokenKind::DocBlockStart
            | DocTokenKind::BlockCommentStart
            | DocTokenKind::BlockCommentEnd => CppTokenKind::BlockComment,
            DocTokenKind::DocCommandName => CppTokenKind::DocCommandName,
            DocTokenKind::DocText => CppTokenKind::DocText,
            DocTokenKind::DocWhitespace => CppTokenKind::Whitespace,
            DocTokenKind::DocNewline => CppTokenKind::Newline,
            DocTokenKind::DocIntroducer
            | DocTokenKind::DocLeftBracket
            | DocTokenKind::DocRightBracket
            | DocTokenKind::DocComma
            | DocTokenKind::DocLeftParen
            | DocTokenKind::DocRightParen
            | DocTokenKind::DocDot
            | DocTokenKind::DocColon
            | DocTokenKind::DocLess
            | DocTokenKind::DocGreater
            | DocTokenKind::DocEquals
            | DocTokenKind::DocScope => CppTokenKind::DocTrivia,
            DocTokenKind::Eof => CppTokenKind::Eof,
        }
    }

    /// Is this token layout rather than content? The doc grammar steps over these.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            DocTokenKind::DocWhitespace | DocTokenKind::DocNewline
        )
    }

    /// Does this token end the line it is on? Used by the line-oriented parsers.
    pub fn ends_a_line(self) -> bool {
        matches!(self, DocTokenKind::DocNewline | DocTokenKind::Eof)
    }
}
