use rowan::{GreenNode, NodeCache};

use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    text::SourceRange,
};

/// One node's worth of already-parsed children.
///
/// The builder keeps a tree of these rather than an index arena: every `finish_node` consumes the
/// children pushed since the matching `start_node` and hands back a single element, so the
/// structure cannot drift out of sync with the parser's marker stack. An index-based arena makes
/// that bookkeeping implicit and an off-by-one there produces a tree that is still well-formed but
/// wrongly nested — the exact failure the parser's marker stack exists to prevent, so it must not
/// be reintroduced here.
#[derive(Debug, Clone)]
enum CppGreenElement {
    Node {
        kind: CppSyntaxKind,
        children: Vec<CppGreenElement>,
    },
    Token {
        kind: CppTokenKind,
        range: SourceRange,
    },
}

/// A builder for a rowan green tree specialised for C++.
///
/// Tokens carry [`SourceRange`]s into the original text instead of the text itself, so the original
/// file is the single source of truth and the tree stays perfectly lossless: the builder never
/// invents, drops or re-spans a token.
///
/// Whitespace and comment placement is decided by the parser (`CppParser::bump` eats trivia into
/// whichever node is currently open), so unlike the Lua-era builder this one performs no trivia
/// re-parenting pass — it only folds the flat element list into a tree.
#[derive(Default, Debug)]
pub struct CppGreenNodeBuilder<'cache> {
    /// Kinds of the nodes that are currently open, innermost last.
    open_nodes: Vec<CppSyntaxKind>,
    /// Children collected so far for each open node, parallel to `open_nodes`.
    open_children: Vec<Vec<CppGreenElement>>,
    /// Completed top-level elements, in document order.
    roots: Vec<CppGreenElement>,
    builder: rowan::GreenNodeBuilder<'cache>,
}

impl CppGreenNodeBuilder<'_> {
    /// Creates new builder.
    pub fn new() -> CppGreenNodeBuilder<'static> {
        CppGreenNodeBuilder::default()
    }

    pub fn with_cache(cache: &mut NodeCache) -> CppGreenNodeBuilder<'_> {
        CppGreenNodeBuilder {
            open_nodes: Vec::new(),
            open_children: Vec::new(),
            roots: Vec::new(),
            builder: rowan::GreenNodeBuilder::with_cache(cache),
        }
    }

    #[inline]
    pub fn token(&mut self, kind: CppTokenKind, range: SourceRange) {
        self.push(CppGreenElement::Token { kind, range });
    }

    #[inline]
    pub fn start_node(&mut self, kind: CppSyntaxKind) {
        self.open_nodes.push(kind);
        self.open_children.push(Vec::new());
    }

    #[inline]
    pub fn finish_node(&mut self) {
        let Some(kind) = self.open_nodes.pop() else {
            // Unbalanced `finish_node` from a caller. The parser's marker stack makes this
            // unreachable; ignoring it here keeps a buggy caller from corrupting the tree.
            debug_assert!(false, "finish_node called with no open node");
            return;
        };

        let children = self
            .open_children
            .pop()
            .expect("open_nodes and open_children are pushed and popped together");
        self.push(CppGreenElement::Node { kind, children });
    }

    fn push(&mut self, element: CppGreenElement) {
        match self.open_children.last_mut() {
            Some(children) => children.push(element),
            None => self.roots.push(element),
        }
    }

    /// Number of nodes still open. The tree is only complete when this is zero.
    pub fn open_node_count(&self) -> usize {
        self.open_nodes.len()
    }

    #[inline]
    pub fn finish(mut self, text: &str) -> GreenNode {
        // A caller that left nodes open has a bug, but the right response is still to produce the
        // best tree we can: the parse is already lossless, and returning a truncated tree would
        // turn a shape bug into data loss. The tree builder balances the event stream before it
        // gets here, so this is a last-resort net.
        debug_assert!(
            self.open_nodes.is_empty(),
            "finishing a green tree with {} node(s) still open; the tree would be wrongly nested",
            self.open_nodes.len()
        );

        // Close whatever is still open, innermost first, so the elements collected under them are
        // not dropped on the floor.
        while let Some(kind) = self.open_nodes.pop() {
            let children = self.open_children.pop().unwrap_or_default();
            self.push(CppGreenElement::Node { kind, children });
        }
        self.open_children.clear();

        // The parser always opens a translation unit first, but the builder must not depend on
        // that: wrap the roots if the events did not produce one.
        let already_rooted = matches!(
            self.roots.as_slice(),
            [CppGreenElement::Node {
                kind: CppSyntaxKind::TranslationUnit,
                ..
            }]
        );

        if already_rooted {
            let root = self.roots.pop().expect("checked just above");
            emit_element(&mut self.builder, root, text);
        } else {
            self.builder
                .start_node(CppSyntaxKind::TranslationUnit.into());
            for root in std::mem::take(&mut self.roots) {
                emit_element(&mut self.builder, root, text);
            }
            self.builder.finish_node();
        }

        self.builder.finish()
    }
}

/// Emit `element` into `builder`, descending into nodes.
///
/// The emitted call sequence corresponds one-to-one with the element tree: one `start_node` and one
/// `finish_node` per node, in document order. Nothing here does any index arithmetic.
fn emit_element(builder: &mut rowan::GreenNodeBuilder<'_>, element: CppGreenElement, text: &str) {
    match element {
        CppGreenElement::Node { kind, children } => {
            builder.start_node(kind.into());

            for child in children {
                emit_element(builder, child, text);
            }

            builder.finish_node();
        }
        CppGreenElement::Token { kind, range } => {
            let start = range.start_offset;
            let end = range.end_offset();
            // The lexer only ever emits ranges that are on `char` boundaries inside `text`, so
            // this slicing cannot panic for a well-formed token stream. If it ever does, that is a
            // lexer bug and we want it to be loud rather than to silently build a corrupt tree.
            builder.token(kind.into(), &text[start..end]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders a green tree as `Kind(child child)`, so a test can assert on *nesting* and not just
    /// on the flattened token text. Nesting is the property that silently breaks.
    fn shape(node: &rowan::GreenNodeData) -> String {
        let mut out = String::new();
        for child in node.children() {
            match child {
                rowan::NodeOrToken::Node(n) => {
                    out.push_str(&format!("{:?}(", n.kind()));
                    out.push_str(&shape(n));
                    out.push(')');
                }
                rowan::NodeOrToken::Token(t) => out.push_str(&format!("{:?}", t.text())),
            }
        }
        out
    }

    #[test]
    fn nests_children_under_their_node() {
        let text = "ab";
        let mut builder = CppGreenNodeBuilder::new();
        builder.start_node(CppSyntaxKind::TranslationUnit);
        builder.token(CppTokenKind::Identifier, SourceRange::new(0, 1));
        builder.start_node(CppSyntaxKind::DeclStat);
        builder.token(CppTokenKind::Identifier, SourceRange::new(1, 1));
        builder.finish_node();
        builder.finish_node();

        let green = builder.finish(text);

        // "a" must be a *sibling* of DeclStat, and "b" must be *inside* it. A flat tree here would
        // mean the builder lost the nesting, which is the bug this assertion exists to catch.
        assert_eq!(
            shape(&green),
            r#""a"SyntaxKind(32794)("b")"#,
            "children were not nested under their node"
        );
        assert_eq!(green.kind(), CppSyntaxKind::TranslationUnit.into());
    }

    #[test]
    fn wraps_bare_tokens_in_a_translation_unit() {
        let text = "int x;";
        let mut builder = CppGreenNodeBuilder::new();
        builder.token(CppTokenKind::IntKeyword, SourceRange::new(0, 3));
        builder.token(CppTokenKind::Whitespace, SourceRange::new(3, 1));
        builder.token(CppTokenKind::Identifier, SourceRange::new(4, 1));
        builder.token(CppTokenKind::Semicolon, SourceRange::new(5, 1));
        let green = builder.finish(text);

        assert_eq!(green.kind(), CppSyntaxKind::TranslationUnit.into());
        assert_eq!(usize::from(green.text_len()), text.len());
        assert_eq!(green.children().len(), 4);
    }

    #[test]
    fn does_not_double_wrap_an_existing_root() {
        let text = "int";
        let mut builder = CppGreenNodeBuilder::new();
        builder.start_node(CppSyntaxKind::TranslationUnit);
        builder.token(CppTokenKind::IntKeyword, SourceRange::new(0, 3));
        builder.finish_node();
        let green = builder.finish(text);

        assert_eq!(green.kind(), CppSyntaxKind::TranslationUnit.into());
        // The translation unit is the root itself, not wrapped in a second translation unit.
        assert_eq!(shape(&green), r#""int""#);
    }

    #[test]
    fn empty_input_still_yields_a_root() {
        let green = CppGreenNodeBuilder::new().finish("");
        assert_eq!(green.kind(), CppSyntaxKind::TranslationUnit.into());
        assert_eq!(green.children().len(), 0);
    }

    #[test]
    fn multiple_roots_are_wrapped_in_order() {
        let text = "ab";
        let mut builder = CppGreenNodeBuilder::new();
        builder.token(CppTokenKind::Identifier, SourceRange::new(0, 1));

        builder.start_node(CppSyntaxKind::DeclStat);
        builder.token(CppTokenKind::Identifier, SourceRange::new(1, 1));
        builder.finish_node();

        let green = builder.finish(text);
        assert_eq!(shape(&green), r#""a"SyntaxKind(32794)("b")"#);
    }
}
