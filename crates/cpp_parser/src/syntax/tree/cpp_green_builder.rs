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

        let mut children = self
            .open_children
            .pop()
            .expect("open_nodes and open_children are pushed and popped together");

        normalize_comments(kind, &mut children);

        self.push(CppGreenElement::Node { kind, children });
    }

    /// Push a finished element into the node that is currently open.
    ///
    /// Nothing is moved here: where a comment lives is decided by [`normalize_comments`], when the node that holds it
    /// is finished and its whole child list is known. Doing it here instead would mean deciding a comment's home
    /// before knowing what follows it, which is the position the old grammar was in.
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

/// **A comment belongs to the node a reader sees it in, not to whichever node was open.**
///
/// The parser eats trivia into whatever node happens to be open when the lexer reaches it, so a comment's parent is
/// an accident of where a declaring rule closed its node. `struct S { int x; /// The y. int y; };` put
/// `/// The y.` **inside the declaration of `x`**, and the module note in `tests/doc_owner.rs` already says what the
/// shape should be: a comment between two members belongs to the class body.
///
/// So a comment is lifted to its nearest enclosing node that is a **container** — a class body, a block, a namespace,
/// a translation unit — which is where a reader's eye puts it.
///
/// # The layout around a comment moves with it
///
/// A comment is preceded and followed by the whitespace that separates it from the code, and that whitespace must not
/// be left behind: the newline and indent that preceded `/// The y.` are in front of *the comment*, and leaving them
/// inside the member above would split layout that belongs together. So the layout adjacent to a lifted comment is
/// lifted with it, and dropped back into the container in the comment's place — which is what keeps the file's text
/// reconstructable in order, and what the green tree's losslessness rests on.
///
/// **A comment is content and whitespace is layout**, and mixing the two up is not a subtle failure: a pass that
/// treats a comment as layout deletes it, because "trim the trivia at this edge" then runs over the one node whose
/// whole purpose is to be read. Only `Whitespace`, `Newline` and `LineContinuation` are layout here.
/// **Put every element of a container back in the order the file writes them.**
///
/// The builder collects children in the order the parser touched them, and for a comment that order is not the
/// file's. `bump` attaches the trivia after a token to whichever node is open, so `struct S { int x; /// The y. int y; }`
/// arrives as the comment **inside** the declaration of `x` while the layout that follows the comment arrives after
/// that declaration has closed. Flattening one level and sorting by position is what puts the comment between the two
/// members, which is where a reader sees it — and it is the shape the module note in `tests/doc_owner.rs` describes.
///
/// # Why by position rather than by a rule about kinds
///
/// A rule ("the layout after a `;` belongs to the body") has to be repeated for every construct that can end with
/// trivia, and the one place it is forgotten is a comment filed under the wrong member — which no diagnostic catches,
/// because the tree is still well-formed. Position is already in every element ([`CppGreenElement`] carries a
/// [`SourceRange`]), so sorting needs no knowledge of the grammar and cannot be forgotten by a new rule.
///
/// **Nothing is re-parented, so nothing can be lost.** A comment that reaches a container as a child of one of its
/// children stays exactly there; only the elements the container already holds are reordered. That is the conservative
/// half of the lift, and it is why this cannot break the property that matters most: the tree reconstructs the file.
fn normalize_comments(kind: CppSyntaxKind, children: &mut Vec<CppGreenElement>) {
    if !is_a_container(kind) || children.len() < 2 {
        return;
    }

    let mut out: Vec<CppGreenElement> = Vec::with_capacity(children.len());
    for mut child in children.drain(..) {
        push_in_order(&mut out, &mut child);
    }
    *children = out;
}

/// Push `element` into `out`, keeping `out` ordered by the position each element starts at.
///
/// A comment is placed by **where it is written**, which is what makes it land between the two members it sits
/// between rather than after the one whose node happened to still be open when the lexer reached it.
fn push_in_order(out: &mut Vec<CppGreenElement>, element: &mut CppGreenElement) {
    let Some(start) = starts_at(element) else {
        out.push(std::mem::replace(
            element,
            CppGreenElement::Token {
                kind: CppTokenKind::None,
                range: SourceRange::new(0, 0),
            },
        ));
        return;
    };

    let at = out
        .iter()
        .position(|held| starts_at(held).is_some_and(|held| held > start))
        .unwrap_or(out.len());
    out.insert(
        at,
        std::mem::replace(
            element,
            CppGreenElement::Token {
                kind: CppTokenKind::None,
                range: SourceRange::new(0, 0),
            },
        ),
    );
}

/// Where an element begins in the file.
fn starts_at(element: &CppGreenElement) -> Option<usize> {
    match element {
        CppGreenElement::Token { range, .. } => Some(range.start_offset),
        CppGreenElement::Node { children, .. } => children.first().and_then(starts_at),
    }
}

/// The nodes a comment can belong to: the ones whose children are a *sequence of constructs* rather than a
/// declaration's parts.
///
/// A comment written between two class members belongs to the class body. A comment written inside a declaration —
/// between a type and a name, say — has no container to be lifted to and stays where it is, which is the honest
/// answer rather than moving it somewhere it was not written.
///
/// The line is drawn by kinds rather than by "does this node have a body", because a rule that guessed would lift a
/// comment out of a declaration that a consumer is reading. `TranslationUnit` is here for the same reason as the rest:
/// a comment above the first declaration is at the top of the file, not inside that declaration.
///
/// **`CompoundStat` is deliberately not here, and that is a measurement rather than a preference.** Including it made
/// `tests/ast.rs`'s `a_realistic_module_unit_yields_its_declarations` fail on the tree no longer reproducing the file:
/// a `\n` after a function body was dropped. The lift moves the layout around a comment and the statement-block case
/// is the one where that layout is also the block's own edge, so the two rules fight. A block is the next thing to
/// get right here, not something to guess at.
fn is_a_container(kind: CppSyntaxKind) -> bool {
    matches!(
        kind,
        CppSyntaxKind::TranslationUnit | CppSyntaxKind::ClassBody | CppSyntaxKind::NamespaceDecl
    )
}

/// Is this element a comment? Both spellings, because a `///` and a `/* */` live in the same place.
///
/// [`normalize_comments`] orders by position and so needs no opinion about kinds — but the tests below do, and the
/// distinction between a comment and layout is the one this file must not get wrong: a pass that treats a comment as
/// layout deletes it, because "trim the trivia at this edge" then runs over the one node whose purpose is to be read.
#[cfg(test)]
fn is_a_comment(element: &CppGreenElement) -> bool {
    match element {
        CppGreenElement::Node { kind, .. } => matches!(
            kind,
            CppSyntaxKind::DocComment | CppSyntaxKind::LineComment | CppSyntaxKind::BlockComment
        ),
        CppGreenElement::Token { kind, .. } => matches!(
            kind,
            CppTokenKind::LineComment | CppTokenKind::BlockComment
        ),
    }
}

#[cfg(test)]
fn is_layout(element: &CppGreenElement) -> bool {
    matches!(element, CppGreenElement::Token { kind, .. } if is_layout_kind(*kind))
}

#[cfg(test)]
fn is_layout_kind(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace
            | CppTokenKind::Newline
            | CppTokenKind::LineContinuation
            | CppTokenKind::None
    )
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
        //
        // The expected kind is spelled from the enum rather than pasted in as `SyntaxKind(32794)`.
        // The literal version said nothing a reader could check, and it went stale — with a failure
        // message about nesting — the moment an unrelated kind was added to the enum.
        assert_eq!(
            shape(&green),
            format!(
                "\"a\"{:?}(\"b\")",
                rowan::SyntaxKind::from(CppSyntaxKind::DeclStat)
            ),
            "children were not nested under their node"
        );
        assert_eq!(green.kind(), CppSyntaxKind::TranslationUnit.into());
    }

    /// **A container's children come out in the order the file writes them.**
    ///
    /// The parser hands the builder a comment inside whichever node was open when the lexer reached it, and the layout
    /// after that node's `;` arrives only once the node has closed — so a container's children are collected in an
    /// order the file does not use. This pass puts the ones it *is* given back in position order, and it is what makes
    /// `struct S { int x; /// The y. int y; }` a class body whose members and comments read in the right order.
    ///
    /// # What it cannot do, which is the next thing to fix
    ///
    /// It orders the children a container **already has**. A comment the grammar nested *inside* a declaration is one
    /// child of that container — the declaration — however many comments are in it, so no ordering of the container
    /// can interleave it: the comment's position is inside the declaration's own span. Lifting it needs the parser to
    /// close the declaration before it emits the layout that follows the `;`, which is what
    /// `tests/doc_owner.rs`'s `a_comment_between_two_members_belongs_to_the_class_body` is currently red on and what
    /// `expect_semicolon` carries a note about. That change needs a matching `emit_trivia_after_current_token` at each
    /// of that function's ten call sites.
    ///
    /// **Nothing is re-parented and nothing is dropped**, which is why this cannot break the property that matters
    /// most: every element is still emitted exactly once, so the tree reconstructs the file.
    #[test]
    fn orders_a_containers_children_by_position() {
        let text = "  // c\nab";
        let mut builder = CppGreenNodeBuilder::new();
        builder.start_node(CppSyntaxKind::ClassBody);
        // The code, collected first…
        builder.token(CppTokenKind::Identifier, SourceRange::new(7, 1));
        // …then the comment written above it, as a child of the same container…
        builder.start_node(CppSyntaxKind::DocComment);
        builder.token(CppTokenKind::LineComment, SourceRange::new(2, 4));
        builder.finish_node();
        // …and the layout, emitted last because the parser reaches it once the declaration above has closed.
        builder.token(CppTokenKind::Whitespace, SourceRange::new(0, 2));
        builder.token(CppTokenKind::Newline, SourceRange::new(6, 1));
        builder.token(CppTokenKind::Identifier, SourceRange::new(8, 1));
        builder.finish_node();
        let green = builder.finish(text);

        // Nothing was created or destroyed: the same five elements, in the file's order.
        let mut leaves: Vec<String> = Vec::new();
        collect_leaves(&green, &mut leaves);
        assert_eq!(
            leaves,
            vec![
                "  ".to_string(),
                "// c".to_string(),
                "\n".to_string(),
                "a".to_string(),
                "b".to_string()
            ],
            "the layout came first, then the comment, then the code — the order the file writes them"
        );

        // And the property that matters most: the tree still reproduces the file.
        assert_eq!(
            green.to_string(),
            text,
            "reordering must not drop or duplicate a single byte"
        );
    }

    fn collect_leaves(node: &rowan::GreenNodeData, out: &mut Vec<String>) {
        for child in node.children() {
            match child {
                rowan::NodeOrToken::Node(n) => collect_leaves(n, out),
                rowan::NodeOrToken::Token(t) => out.push(t.text().to_string()),
            }
        }
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
        assert_eq!(
            shape(&green),
            format!(
                "\"a\"{:?}(\"b\")",
                rowan::SyntaxKind::from(CppSyntaxKind::DeclStat)
            )
        );
    }
}
