use rowan::GreenNode;

use crate::{
    CppAstNode, CppTranslationUnit, kind::CppSyntaxKind, lexer::CppTokenData,
    parser_error::CppParseError, syntax::CppSyntaxNode,
};

/// The parse result for one source file.
///
/// Holds a `GreenNode` (not a red `SyntaxNode`) because `SyntaxNode` is neither `Send` nor `Sync`,
/// while the green tree is immutable and freely shareable across threads — the LSP layer relies
/// on that. Call [`CppSyntaxTree::get_red_root`] to obtain a typed cursor into the tree.
///
/// It also carries the **token stream it was parsed from**. That is not a convenience: the token
/// stream is the primary artifact and the tree is one view of it — every directive, every region and
/// every query that needs positions rather than structure reads the tokens, and re-lexing to get them
/// would be a second answer to a question that already has one. The vector is `Clone`d with the tree
/// and costs about twelve bytes per token.
#[derive(Debug, Clone)]
pub struct CppSyntaxTree {
    root: GreenNode,
    errors: Vec<CppParseError>,
    tokens: Vec<CppTokenData>,
}

impl CppSyntaxTree {
    pub fn new(root: GreenNode, errors: Vec<CppParseError>, tokens: Vec<CppTokenData>) -> Self {
        CppSyntaxTree {
            root,
            errors,
            tokens,
        }
    }

    /// Every token of the file, in order, trivia included — the stream this tree was parsed from.
    ///
    /// Losslessness is a property of this stream as much as of the tree: concatenating each token's
    /// text reproduces the file byte for byte.
    pub fn get_tokens(&self) -> &[CppTokenData] {
        &self.tokens
    }

    pub fn get_red_root(&self) -> CppSyntaxNode {
        CppSyntaxNode::new_root(self.root.clone())
    }

    pub fn get_green_root(&self) -> &GreenNode {
        &self.root
    }

    pub fn get_unit(&self) -> CppTranslationUnit {
        CppTranslationUnit::cast(self.get_red_root()).unwrap()
    }

    pub fn get_errors(&self) -> &[CppParseError] {
        &self.errors
    }

    pub fn has_syntax_errors(&self) -> bool {
        self.errors
            .iter()
            .any(|e| e.kind == crate::parser_error::CppParseErrorKind::SyntaxError)
    }

    /// Total byte length of the tree. Used by tests to assert losslessness against the input.
    pub fn text_len(&self) -> usize {
        usize::from(self.root.text_len())
    }

    /// Root kind, mainly for assertions and for the `finish` sanity check in the tree builder.
    pub fn root_kind(&self) -> CppSyntaxKind {
        crate::kind::CppKind::from_raw(self.root.kind().0).into()
    }

    /// Reconstructs the original text by concatenating every token in the tree, in order.
    ///
    /// This is the executable form of invariant **I1 (losslessness)**: for any input,
    /// `tree.to_source_text()` must equal the input byte for byte. Tests rely on it heavily.
    pub fn to_source_text(&self) -> String {
        let mut out = String::with_capacity(self.text_len());
        for element in self.get_red_root().descendants_with_tokens() {
            if let rowan::NodeOrToken::Token(token) = element {
                out.push_str(token.text());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstructed_text_equals_input() {
        let tree = CppSyntaxTree::new(
            {
                let mut builder = rowan::GreenNodeBuilder::new();
                builder.start_node(CppSyntaxKind::TranslationUnit.into());
                builder.token(crate::kind::CppTokenKind::IntKeyword.into(), "int");
                builder.token(crate::kind::CppTokenKind::Whitespace.into(), " ");
                builder.token(crate::kind::CppTokenKind::Identifier.into(), "x");
                builder.token(crate::kind::CppTokenKind::Semicolon.into(), ";");
                builder.finish_node();
                builder.finish()
            },
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(tree.to_source_text(), "int x;");
        assert_eq!(tree.root_kind(), CppSyntaxKind::TranslationUnit);
    }
}
