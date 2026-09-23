//! A token as the analysis layer sees it: owned text, file coordinates, and nothing else.
//!
//! The syntax tree stores tokens as ranges into the file, which is what makes it lossless and cheap.
//! That is the wrong shape for the analysis layer to work in, for two reasons:
//!
//! * **Expansion produces tokens that are in no file.** A `#define` body pasted into a call site, or a
//!   `##` joining two spellings, has no range to point at. Anything holding a `SourceRange` would have
//!   to invent one.
//! * **The tree is borrowed; analysis results are not.** A resolved macro table outlives the parse it
//!   came from — that is the point of caching it — so it cannot borrow a `&str` that the tree owns.
//!
//! So tokens are flattened once, into owned `Box<str>` text. A directive is a few dozen tokens at
//! most, and there are as many directives as there are lines beginning with `#`, so the copying is
//! bounded by the number of directives rather than by the size of the file.

use cpp_parser::{CppKind, CppSyntaxNode, CppSyntaxToken, CppTokenKind, SourceRange};

/// One token, with its text detached from the tree.
///
/// Not `Hash`: [`SourceRange`] is not, and a token is not a key in anything — the macro table is keyed
/// by name, and directives by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: CppTokenKind,
    /// The text as written, including any surrounding quotes.
    pub text: Box<str>,
    /// Where it came from in the file.
    ///
    /// Always present: a token produced by *expansion* is not one of these — it is a member of a
    /// macro body, which is itself a sequence of these, each of which was written somewhere.
    pub range: SourceRange,
}

impl Token {
    pub fn new(kind: CppTokenKind, text: impl Into<Box<str>>, range: SourceRange) -> Self {
        Token {
            kind,
            text: text.into(),
            range,
        }
    }

    /// Read a token out of the tree.
    pub fn from_syntax(token: &CppSyntaxToken) -> Self {
        Token {
            kind: CppTokenKind::from(token.kind()),
            text: token.text().into(),
            range: cpp_parser::source_range(token.text_range()),
        }
    }

    pub fn is(&self, kind: CppTokenKind) -> bool {
        self.kind == kind
    }

    pub fn is_identifier(&self) -> bool {
        self.kind == CppTokenKind::Identifier
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The token's text as an integer, for the places a number is written as a directive argument.
    ///
    /// A `#line` argument is always decimal; a `#if` operand is not, and goes through the expression
    /// evaluator instead. This is the narrow helper, not a general literal reader.
    pub fn as_decimal(&self) -> Option<u64> {
        self.text.parse().ok()
    }
}

/// Every token of a node, in order, trivia included.
///
/// Trivia is kept rather than filtered because directive parsing needs it: whether `MAX` in
/// `#define MAX(x)` is function-like depends on whether a `(` follows it *immediately*, and "immediately"
/// is a question about the gap between two tokens. A caller that wants the significant tokens filters
/// them afterwards, with [`is_trivia`](cpp_parser::is_trivia).
pub fn tokens_of(node: &CppSyntaxNode) -> Vec<Token> {
    node.children_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| Token::from_syntax(&token))
        .collect()
}

/// Is this token invisible to the grammar (whitespace, newline, splice, comment)?
pub fn is_trivia(kind: CppTokenKind) -> bool {
    cpp_parser::is_trivia(kind)
}

/// Does this token end a logical line?
///
/// A `LineContinuation` does not: `#define FOO \<newline> bar` is one directive, which is exactly why
/// the lexer gives the splice a kind of its own instead of folding it into whitespace.
pub fn ends_logical_line(kind: CppTokenKind) -> bool {
    matches!(kind, CppTokenKind::Newline | CppTokenKind::Eof)
}

/// Is this the `#` that begins a directive, as opposed to a `#` inside a macro body?
pub fn is_hash(kind: CppTokenKind) -> bool {
    kind == CppTokenKind::Hash
}

/// Is this token a keyword — a word C++ reserves, but which is an ordinary word inside a directive?
///
/// `#define private public` and `#define true 1` are used in real test suites, and neither means
/// anything special in a directive. The lexer has no way to know it is inside one, so this is the test
/// that recovers the distinction.
pub fn is_keyword_like(kind: CppTokenKind) -> bool {
    cpp_parser::is_keyword(kind)
}

/// The `CppKind` of a token kind, for callers that need to compare against tree kinds.
pub fn kind_of(kind: CppTokenKind) -> CppKind {
    CppKind::Token(kind)
}
