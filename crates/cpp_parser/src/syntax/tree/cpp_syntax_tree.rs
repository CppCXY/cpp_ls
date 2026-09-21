use rowan::GreenNode;

use crate::{
    kind::CppSyntaxKind,
    parser_error::CppParseError,
    syntax::CppSyntaxNode,
};

/// The parse result for one source file.
///
/// Holds a `GreenNode` (not a red `SyntaxNode`) because `SyntaxNode` is neither `Send` nor `Sync`,
/// while the green tree is immutable and freely shareable across threads — the LSP layer relies
/// on that. Call [`CppSyntaxTree::get_red_root`] to obtain a typed cursor into the tree.
#[derive(Debug, Clone)]
pub struct CppSyntaxTree {
    root: GreenNode,
    errors: Vec<CppParseError>,
}

impl CppSyntaxTree {
    pub fn new(root: GreenNode, errors: Vec<CppParseError>) -> Self {
        CppSyntaxTree { root, errors }
    }

    pub fn get_red_root(&self) -> CppSyntaxNode {
        CppSyntaxNode::new_root(self.root.clone())
    }

    pub fn get_green_root(&self) -> &GreenNode {
        &self.root
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
        );

        assert_eq!(tree.to_source_text(), "int x;");
        assert_eq!(tree.root_kind(), CppSyntaxKind::TranslationUnit);
    }
}
