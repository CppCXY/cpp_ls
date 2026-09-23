//! One file, analysed: its tokens, its macros, its conditionals.
//!
//! This is the entry point a consumer uses. The layers below it are separable on purpose — a caller that
//! only wants a file's macros should not have to build its token list — but nobody writing a feature
//! wants to assemble them by hand, and the order they have to be assembled in is not obvious:
//!
//! * the **macro table** is built by walking the directives, and it is position-dependent, so it cannot
//!   be built without the file's text;
//! * **expanding a region** needs the token list *and* the table *as of that region* — a `#define` later
//!   in the file must not apply to a use before it;
//! * the **token list** has to come from the tree rather than from the lexer, or it would disagree with
//!   the parse about where a comment begins: the documentation layer re-lexes comments into finer tokens,
//!   and a consumer that lexed the file itself would see one `LineComment` where the tree has five
//!   tokens, and would report positions the rest of the analysis does not use.
//!
//! # What a file is *not*
//!
//! A translation unit. `#include` is not followed here — that is the include graph's job, and it needs a
//! search path and a filesystem. What this provides is everything that is decidable from one file's text,
//! which is deliberately the majority of what an editor needs on a keystroke.

use cpp_parser::{CppSyntaxNode, CppSyntaxTree, SourceRange};

use crate::{
    expand::{Expansion, expand},
    preprocess::{FilePreprocessing, preprocess},
    token::Token,
};

/// Every token of a file, in source order, in file coordinates.
///
/// Taken from the **tree** rather than from the lexer. The two agree about most of a file and disagree
/// about comments, which is the point: the documentation layer replaces a comment with the tokens it is
/// made of, so `// x` is five tokens in the tree and one in the lexer. A consumer using the lexer's view
/// would place every position after a comment differently from everything else in the analysis.
#[derive(Debug, Clone)]
pub struct FileTokens {
    source: Box<str>,
    tokens: Vec<Token>,
}

impl FileTokens {
    /// Read a file's tokens out of its tree.
    pub fn from_tree(source: &str, tree: &CppSyntaxTree) -> Self {
        let tokens = tree
            .get_red_root()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .map(|token| Token::from_syntax(&token))
            .collect();

        FileTokens {
            source: source.into(),
            tokens,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// The tokens whose range lies inside `range`, in order.
    ///
    /// A token that only *overlaps* the range is excluded rather than clipped. Clipping would have to
    /// rewrite the token's text, and a token whose text disagrees with its range is the one thing this
    /// layer works hard to avoid everywhere else.
    pub fn in_range(&self, range: SourceRange) -> Vec<Token> {
        self.tokens
            .iter()
            .filter(|token| {
                token.range.start_offset >= range.start_offset
                    && token.range.end_offset() <= range.end_offset()
            })
            .cloned()
            .collect()
    }

    /// The token containing `offset`, if any.
    ///
    /// The lookup a "what is under the cursor" feature needs. Binary search, because a file has as many
    /// tokens as it has characters and this runs on every cursor movement.
    pub fn token_at(&self, offset: usize) -> Option<&Token> {
        let index = self
            .tokens
            .partition_point(|token| token.range.end_offset() <= offset);

        self.tokens
            .get(index)
            .filter(|token| token.range.start_offset <= offset)
    }

    /// The span the file's tokens cover, if it has any.
    pub fn span(&self) -> Option<SourceRange> {
        let first = self.tokens.first()?;
        let last = self.tokens.last()?;

        Some(SourceRange::new(
            first.range.start_offset,
            last.range.end_offset() - first.range.start_offset,
        ))
    }
}

/// A file, with everything that is decidable from its own text.
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    pub tokens: FileTokens,
    pub preprocessing: FilePreprocessing,
}

impl FileAnalysis {
    /// Analyse a file.
    pub fn new(source: &str, tree: &CppSyntaxTree) -> Self {
        FileAnalysis {
            tokens: FileTokens::from_tree(source, tree),
            preprocessing: preprocess(&tree.get_red_root()),
        }
    }

    /// Analyse a file, parsing it here.
    ///
    /// For callers that have no tree yet. A caller that already parsed the file should use
    /// [`new`](Self::new), because parsing twice is the most expensive thing this layer can do.
    pub fn parse(source: &str, config: cpp_parser::ParserConfig<'_>) -> Self {
        let tree = cpp_parser::CppParser::parse(source, config);
        FileAnalysis::new(source, &tree)
    }

    /// Expand the macros in `range`, as of the macros in force at that range.
    ///
    /// The two halves of that sentence are why this method exists rather than a free function: the macro
    /// table is position-dependent, so a caller that expanded a region against the file's *final* table
    /// would expand a use with a definition written after it — silently, and only in files where a macro
    /// is defined twice.
    pub fn expand_range(&self, range: SourceRange) -> Expansion {
        let tokens = self.tokens.in_range(range);
        expand(&tokens, &self.preprocessing.macros_at(range.start_offset))
    }

    /// Expand the macros in a node's range.
    pub fn expand_node(&self, node: &CppSyntaxNode) -> Expansion {
        self.expand_range(cpp_parser::source_range(node.text_range()))
    }

    /// Expand one line, identified by an offset inside it.
    ///
    /// What a hover needs: the cursor is somewhere on a line, and the line is what the user is asking
    /// about. The range is computed over the file's own text so that a line ending at the end of the file
    /// with no newline is still a line.
    pub fn expand_line_at(&self, offset: usize) -> Expansion {
        let span = self.line_range_at(offset);
        self.expand_range(span)
    }

    /// The range of the line containing `offset`, including its line ending.
    pub fn line_range_at(&self, offset: usize) -> SourceRange {
        let source = self.tokens.source();
        let offset = offset.min(source.len());

        let start = source[..offset]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let end = source[offset..]
            .find('\n')
            .map(|index| offset + index + 1)
            .unwrap_or(source.len());

        SourceRange::new(start, end - start)
    }
}

// The parts of this layer: the analysis itself lives in this module, and `token` is the token of the *expanded*
// stream — the one that knows which macro produced it.
pub mod token;
