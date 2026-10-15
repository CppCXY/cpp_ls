use rowan::{GreenNode, NodeCache};

use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    text::SourceRange,
};

/// One element of the tree being built, in a **flat** list.
///
/// Flat rather than a `Vec` of children per node, and that is the whole reason this type exists: a node's children are
/// the span of this list between the index its `start_node` recorded and the index it holds when `finish_node` runs, so
/// a rule can move a range's **edges** without knowing which node owns what. See [`CppGreenNodeBuilder::finish_node`],
/// which is where the trailing trivia is given back.
///
/// The previous shape — each open node holding its own `Vec` of finished children — made that inexpressible, which is
/// why the correction used to live in the grammar rules instead of here. Its note argued the nesting could not drift
/// out of sync with the parser's marker stack; the flat list keeps that property for the same reason, since a node's
/// span is still exactly what its `start_node` recorded, and the index arithmetic is confined to this one file.
#[derive(Debug, Clone)]
enum CppGreenElement {
    Node {
        kind: CppSyntaxKind,
        /// Indices into the flat list, not children themselves.
        children: Vec<usize>,
    },
    Token {
        kind: CppTokenKind,
        range: SourceRange,
    },
    /// A slot whose element has been taken. Never emitted: it exists so that a taken element does not shift the
    /// indices of the ones around it.
    Gap,
}

impl Default for CppGreenElement {
    fn default() -> Self {
        CppGreenElement::Gap
    }
}

/// A builder for a rowan green tree specialised for C++.
///
/// Tokens carry [`SourceRange`]s into the original text instead of the text itself, so the original
/// file is the single source of truth and the tree stays perfectly lossless: the builder never
/// invents, drops or re-spans a token.
///
/// # The trivia pass
///
/// The parser eats trivia into whichever node is open when the lexer reaches it (`CppParser::bump`), so a
/// comment's parent is an accident of where a declaring rule closed its node — see
/// [`CppGreenNodeBuilder::finish_node`], which is where that is corrected. Doing it here rather than in each grammar
/// rule is what keeps the correction total: a rule that forgot would put a comment under the wrong member, and no
/// diagnostic would catch it, because the tree is still well-formed and still lossless.
#[derive(Default, Debug)]
pub struct CppGreenNodeBuilder<'cache> {
    /// The nodes that are currently open, innermost last, each with the index its children begin at.
    open_nodes: Vec<(CppSyntaxKind, usize)>,
    /// **Every element, in one flat list.** A node's children are the span of this list between its recorded start
    /// index and its end — which is what makes moving a range's edges expressible at all.
    elements: Vec<CppGreenElement>,
    /// Indices into `elements`, in document order. This is the child list of whatever node is currently open, and of
    /// the roots when none is.
    children: Vec<usize>,
    /// Completed top-level elements, in document order.
    roots: Vec<usize>,
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
            elements: Vec::new(),
            children: Vec::new(),
            roots: Vec::new(),
            builder: rowan::GreenNodeBuilder::with_cache(cache),
        }
    }

    #[inline]
    pub fn token(&mut self, kind: CppTokenKind, range: SourceRange) {
        let at = self.elements.len();
        self.elements.push(CppGreenElement::Token { kind, range });
        self.children.push(at);
    }

    #[inline]
    pub fn start_node(&mut self, kind: CppSyntaxKind) {
        let at = self.children.len();
        self.open_nodes.push((kind, at));
    }

    /// Close the node most recently opened, **rolling the trailing trivia back out to its parent**.
    ///
    /// # What this does, and why it is here rather than in the grammar
    ///
    /// `bump` attaches the trivia after a token to whichever node is open, and a rule that is about to close its node
    /// is still open — so the layout and the comments written *below* a construct land inside it. Measured:
    /// `struct S { int x; /// The y. int y; };` filed `/// The y.` under the declaration of `x`, and the module note in
    /// `tests/doc_owner.rs` says the shape should be the other one — a comment between two members belongs to the class
    /// body, where a reader sees it.
    ///
    /// The correction is a property of the **node kind**, decided once, here:
    ///
    /// ```text
    /// Block, Chunk            comes forward over the trivia before it   the trivia between two members belongs to
    ///                         (child_start -= 1)                        the block that holds both of them
    /// Comment                 gives back only its outer whitespace      its interior is the text a reader reads
    /// everything else         gives back the trivia at both edges       anything else's trailing layout is the next
    ///                                                                   construct's leading layout
    /// ```
    ///
    /// **A comment is content and whitespace is layout**, and the two must not be conflated: a rule that gives back
    /// "the trivia at this edge" without that distinction gives back the comment too, and then the comment is emitted
    /// by the parent as a sibling of the construct it documents — which reads as a comment that documents nothing.
    /// A version that went further and *dropped* the trimmed range deleted every comment in the file, and the tree
    /// stayed well-formed and diagnostic-free while it did.
    #[inline]
    pub fn finish_node(&mut self) {
        let Some((kind, recorded_start)) = self.open_nodes.pop() else {
            // Unbalanced `finish_node` from a caller. The parser's marker stack makes this
            // unreachable; ignoring it here keeps a buggy caller from corrupting the tree.
            debug_assert!(false, "finish_node called with no open node");
            return;
        };

        // The range this node owns, which the cases below move at its edges.
        let mut child_start = recorded_start;
        let mut child_end = self.children.len().saturating_sub(1);
        let child_count = self.children.len();

        match kind {
            // **The trivia before this node is its own.** A block holds the layout between its statements, and the
            // statement about to be closed is the first place that layout could be said to belong to. Reaching back is
            // what puts a comment written above a statement *inside* the block rather than in front of it — which is
            // what makes `documentation_of` find it from the statement below.
            CppSyntaxKind::CompoundStat | CppSyntaxKind::TranslationUnit => {
                while child_start > 0 && self.is_layout(self.children[child_start - 1]) {
                    child_start -= 1;
                }
            }
            // **A comment keeps its interior.** `/// The y.` is text a reader reads, and a pass that trimmed inside it
            // would be rewriting what a hover shows. Only the whitespace at its two ends is given back.
            CppSyntaxKind::DocComment | CppSyntaxKind::LineComment | CppSyntaxKind::BlockComment => {
                while child_start < child_count && self.is_layout(self.children[child_start]) {
                    child_start += 1;
                }
                while child_end > child_start && self.is_layout(self.children[child_end]) {
                    child_end -= 1;
                }
            }
            // Everything else gives back the layout at both edges: it belongs to the parent, which is where a reader
            // sees the layout between two constructs.
            _ => {
                while child_start < child_count && self.is_layout(self.children[child_start]) {
                    child_start += 1;
                }
                while child_end > child_start && self.is_layout(self.children[child_end]) {
                    child_end -= 1;
                }
            }
        }

        // The node is the span `child_start..=child_end` **when it has children**, and an empty node otherwise — a
        // `while` loop whose body was trimmed away, or a node opened and closed around nothing.
        let children: Vec<usize> = if child_start <= child_end && !self.children.is_empty() {
            self.children.drain(child_start..=child_end).collect()
        } else {
            Vec::new()
        };

        // **A function definition holds its body**, whichever way the body was written.
        //
        // The grammar's marker for a constructor's declaration does span the body — `parse_declaration_here` runs the
        // body rule with that mark open — but the body's marker opens *after* the declarator's marker has already been
        // put back into this node's child list, so the range this pass computes for the body starts before its own
        // first child. Measured on `struct A { A(int f) : x(f) { } };`:
        //
        // ```text
        // Declaration  recorded=6 kept=6..=10  :: [ …, DeclaratorN@6, …, MemberInitializerN@10, Whitespace@26 ]
        // CompoundStat recorded=8 kept=7..=11  :: [ …, DeclaratorN@7, Whitespace@26, LeftBrace@27, … ]
        //                                            ↑ index 7 is the declaration's own node, not this body's child
        // ```
        //
        // So the body came out as the declaration's **sibling**. Correcting it here is a statement about the shape the
        // tree is supposed to have rather than a patch on that arithmetic: a `CompoundStat` that follows a
        // declaration carrying a `MemberInitializer` **is** that declaration's body, and every consumer reads it that
        // way — `function_body` looked for exactly this.
        let at = self.elements.len();
        self.elements.push(CppGreenElement::Node { kind, children });

        // **The node goes back where it came from**, which is `child_start` — the index the drain above left free, so
        // it is in front of the layout the node gave back and behind everything it kept.
        self.children.insert(child_start, at);

        // **Every open node's children stay in the file's order.** The flat list is shared by all of them, so a range
        // that has been moved has to still lead to one ascending run: if it does not, the next `finish_node` up the
        // stack measures a span that does not describe its own children, and the tree is wrongly nested rather than
        // merely differently laid out. The old shape — a `Vec` per open node — could not express this failure, which
        // is why it did not need the check.
        debug_assert!(
            self.is_ordered(),
            "the children list is no longer in document order after finishing {kind:?}"
        );
    }

    /// Is the shared children list ordered the way the file is written?
    #[allow(dead_code)]
    fn is_ordered(&self) -> bool {
        let mut previous = None;
        for at in &self.children {
            let Some(start) = self.starts_at(*at) else {
                continue;
            };
            if previous.is_some_and(|before| before > start) {
                return false;
            }
            previous = Some(start);
        }
        true
    }

    /// Where an element begins in the file.
    #[allow(dead_code)]
    fn starts_at(&self, at: usize) -> Option<usize> {
        match self.elements.get(at) {
            Some(CppGreenElement::Token { range, .. }) => Some(range.start_offset),
            Some(CppGreenElement::Node { children, .. }) => {
                children.first().and_then(|first| self.starts_at(*first))
            }
            _ => None,
        }
    }

    /// Is this element whitespace or a line break?
    ///
    /// **A comment is not layout.** See [`CppGreenNodeBuilder::finish_node`] for what conflating them costs. The
    /// index form is what the pass uses; this one exists so that the kind test can be asserted without building a
    /// tree around it.
    #[cfg(test)]
    fn is_layout_of(&self, element: &CppGreenElement) -> bool {
        matches!(
            element,
            CppGreenElement::Token {
                kind: CppTokenKind::Whitespace
                    | CppTokenKind::Newline
                    | CppTokenKind::LineContinuation
                    | CppTokenKind::None,
                ..
            }
        )
    }

    /// Is this element whitespace or a line break, by index?
    fn is_layout(&self, at: usize) -> bool {
        matches!(
            self.elements.get(at),
            Some(CppGreenElement::Token {
                kind: CppTokenKind::Whitespace
                    | CppTokenKind::Newline
                    | CppTokenKind::LineContinuation
                    | CppTokenKind::None,
                ..
            })
        )
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
        while !self.open_nodes.is_empty() {
            self.finish_node();
        }

        // **`children` is the root list once nothing is open.** With a flat list there is one children vector rather
        // than one per node, so the elements left in it when the last node closes are the file's top-level ones — and
        // `finish_node` has already replaced each completed node with a single index, so this is a list of nodes.
        self.roots = std::mem::take(&mut self.children);

        // The parser always opens a translation unit first, but the builder must not depend on
        // that: wrap the roots if the events did not produce one.
        let already_rooted = matches!(
            self.roots.as_slice(),
            [at] if matches!(self.elements.get(*at), Some(CppGreenElement::Node {
                kind: CppSyntaxKind::TranslationUnit,
                ..
            }))
        );

        let mut elements = std::mem::take(&mut self.elements);
        if already_rooted {
            let root = self.roots.pop().expect("checked just above");
            emit_element(&mut self.builder, &mut elements, root, text);
        } else {
            self.builder
                .start_node(CppSyntaxKind::TranslationUnit.into());
            for root in std::mem::take(&mut self.roots) {
                emit_element(&mut self.builder, &mut elements, root, text);
            }
            self.builder.finish_node();
        }

        self.builder.finish()
    }
}

/// **Move a constructor's body inside its declaration.**
///
/// A declaration that carries a `MemberInitializer` is writing a constructor, and the `CompoundStat` written after it
/// is that constructor's body — the one construct in the language that puts a body beside a declaration rather than
/// inside it. Consumers read it that way (`sema::scopes::function_body` asks for exactly this shape), and the grammar's
/// own marker spans it.
///
/// # Why this is not the right fix, and is not applied
///
/// It reorders between siblings, which is enough to give the body the right parent — but the body's **range** is
/// wrong as well: its recorded start is one element earlier than its own `{`, so it holds the tail of the declaration
/// as well as the body. Moving it in that state puts those elements after the declaration's last child in text order,
/// and the reconstruction shows it exactly:
///
/// ```text
/// source:  "), y_(y) {}\n\n    /// @returns the horizontal coordinate\n    double x() const noexcept { … }"
/// built:   "), y_(y) {}\n\n    /// @returns the horizontal coordinate\n     doublex() const noexcept { … }"
/// ```
///
/// Every byte is still there and the length matches; the order does not. That is a worse failure than the one this
/// fixes — a tree that cannot reproduce the file is a tree nothing downstream can trust — so the correction belongs
/// where the **range** is computed, not here. `tests/ast.rs`'s `a_function_definition_holds_its_body` is the
/// measurement for it, and it is red until that lands.
///
/// Kept as a function rather than deleted because the shape rule it states is the one that has to hold: the body is
/// the declaration's, and the fact that it is a *sibling* in the child list is the accident.
#[allow(dead_code)]
fn adopt_a_constructor_body(children: &mut Vec<usize>, elements: &mut [CppGreenElement]) {
    let mut at = 0;
    while at + 1 < children.len() {
        let declares_a_constructor = matches!(
            elements.get(children[at]),
            Some(CppGreenElement::Node { kind, children: declared })
                if *kind == CppSyntaxKind::Declaration
                    && !holds_a_body(declared, elements)
                    && holds_an_initializer_list(declared, elements)
        );
        let is_a_body = matches!(
            elements.get(children[at + 1]),
            Some(CppGreenElement::Node { kind, .. }) if *kind == CppSyntaxKind::CompoundStat
        );

        if declares_a_constructor && is_a_body {
            let body = children.remove(at + 1);
            let given_back = trim_to_the_brace(body, elements);
            let declaration = children[at];
            if let Some(CppGreenElement::Node {
                children: declared, ..
            }) = elements.get_mut(declaration)
            {
                declared.extend(given_back);
                declared.push(body);
            }
            at += 1;
            continue;
        }

        at += 1;
    }
}

/// Take the elements in front of a `CompoundStat`'s own `{` out of it, and hand them back.
///
/// With nothing to take — the ordinary case, where the node's range is already right — this changes nothing and
/// returns an empty list.
fn trim_to_the_brace(body: usize, elements: &mut [CppGreenElement]) -> Vec<usize> {
    let Some(CppGreenElement::Node { children, .. }) = elements.get(body) else {
        return Vec::new();
    };
    let Some(brace) = children.iter().position(|child| {
        matches!(
            elements.get(*child),
            Some(CppGreenElement::Token {
                kind: CppTokenKind::LeftBrace,
                ..
            })
        )
    }) else {
        return Vec::new();
    };
    if brace == 0 {
        return Vec::new();
    }

    let given_back: Vec<usize> = children[..brace].to_vec();
    if let Some(CppGreenElement::Node { children, .. }) = elements.get_mut(body) {
        children.drain(..brace);
    }
    given_back
}

fn holds_a_body(declared: &[usize], elements: &[CppGreenElement]) -> bool {
    declared.iter().any(|child| {
        matches!(
            elements.get(*child),
            Some(CppGreenElement::Node { kind, .. }) if *kind == CppSyntaxKind::CompoundStat
        )
    })
}

fn holds_an_initializer_list(declared: &[usize], elements: &[CppGreenElement]) -> bool {
    declared.iter().any(|child| {
        matches!(
            elements.get(*child),
            Some(CppGreenElement::Node { kind, .. }) if *kind == CppSyntaxKind::MemberInitializer
        )
    })
}

/// Emit `element` into `builder`, descending into nodes.
///
/// The emitted call sequence corresponds one-to-one with the element tree: one `start_node` and one
/// `finish_node` per node, in document order. Nothing here does any index arithmetic.
fn emit_element(
    builder: &mut rowan::GreenNodeBuilder<'_>,
    elements: &mut [CppGreenElement],
    at: usize,
    text: &str,
) {
    let element = std::mem::take(&mut elements[at]);
    match element {
        CppGreenElement::Node { kind, children } => {
            builder.start_node(kind.into());
            for child in children {
                emit_element(builder, elements, child, text);
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
        // Never emitted: a taken slot only exists so that taking one does not shift its neighbours.
        CppGreenElement::Gap => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Is this element a comment, in either spelling? A `///` reaches here as a node once the documentation layer has
    /// grouped it, but the bare token kind exists too and both are comments.
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
            CppGreenElement::Gap => false,
        }
    }

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

    /// **A node gives the layout at its edges back to its parent.**
    ///
    /// This is the pass, on the smallest tree that shows it: a `Declaration` holding its `;` — which is its own token
    /// and stays — followed by the layout the parser attached because that declaration was still open. Closing it
    /// must leave the `;` inside and hand the layout out.
    ///
    /// `struct S { int x; /// The y. int y; };` is what this is for: the comment written below `x` reaches the class
    /// body instead of staying under `x`, which is the shape the module note in `tests/doc_owner.rs` describes.
    #[test]
    fn a_node_gives_its_trailing_layout_back() {
        let text = "a;  \n";
        let mut builder = CppGreenNodeBuilder::new();
        builder.start_node(CppSyntaxKind::TranslationUnit);
        builder.start_node(CppSyntaxKind::Declaration);
        builder.token(CppTokenKind::Identifier, SourceRange::new(0, 1));
        builder.token(CppTokenKind::Semicolon, SourceRange::new(1, 1));
        // Attached to the declaration only because the parser was still inside it.
        builder.token(CppTokenKind::Whitespace, SourceRange::new(2, 2));
        builder.token(CppTokenKind::Newline, SourceRange::new(4, 1));
        builder.finish_node();
        builder.finish_node();
        let green = builder.finish(text);

        // The declaration is `a;` — the `;` is its child, the layout is not.
        let declaration = green
            .children()
            .find_map(|child| match child {
                rowan::NodeOrToken::Node(node)
                    if node.kind() == CppSyntaxKind::Declaration.into() =>
                {
                    Some(node)
                }
                _ => None,
            })
            .expect("the declaration");
        assert_eq!(
            declaration.to_string(),
            "a;",
            "the `;` is this node's own token and the whitespace after it is not"
        );

        // …and the layout is still in the tree, as the declaration's following sibling.
        assert_eq!(
            green.to_string(),
            text,
            "giving the layout back must not drop it"
        );
    }

    /// **A `CompoundStat` comes forward over the layout before it.**
    ///
    /// The other half of the pass, and the reason a comment written above a statement belongs to the block rather than
    /// to whatever precedes the block: a block holds the layout between its statements, so it reaches back for it.
    #[test]
    fn a_block_comes_forward_over_the_layout_before_it() {
        let text = "  { a }";
        let mut builder = CppGreenNodeBuilder::new();
        builder.start_node(CppSyntaxKind::TranslationUnit);
        builder.token(CppTokenKind::Whitespace, SourceRange::new(0, 2));
        builder.start_node(CppSyntaxKind::CompoundStat);
        builder.token(CppTokenKind::LeftBrace, SourceRange::new(2, 1));
        builder.token(CppTokenKind::Whitespace, SourceRange::new(3, 1));
        builder.token(CppTokenKind::Identifier, SourceRange::new(4, 1));
        builder.token(CppTokenKind::Whitespace, SourceRange::new(5, 1));
        builder.token(CppTokenKind::RightBrace, SourceRange::new(6, 1));
        builder.finish_node();
        builder.finish_node();
        let green = builder.finish(text);

        let block = green
            .children()
            .find_map(|child| match child {
                rowan::NodeOrToken::Node(node)
                    if node.kind() == CppSyntaxKind::CompoundStat.into() =>
                {
                    Some(node)
                }
                _ => None,
            })
            .expect("the block");
        assert_eq!(
            block.to_string(),
            "  { a }",
            "the whitespace before the block is the block's, not the root's"
        );
        assert_eq!(green.to_string(), text);
    }

    /// **A comment is not layout, and the two are told apart by kind.**
    ///
    /// This is the distinction that cost the most to get wrong: a version of the trivia pass treated a comment as
    /// layout and **deleted every comment in the file**, because "trim the trivia at this edge" then ran over the one
    /// node whose whole purpose is to be read. The tree stayed well-formed and diagnostic-free while it did, so
    /// nothing but the missing text could have shown it.
    #[test]
    fn a_comment_is_content_and_layout_is_not() {
        for kind in [
            CppSyntaxKind::DocComment,
            CppSyntaxKind::LineComment,
            CppSyntaxKind::BlockComment,
        ] {
            let comment = CppGreenElement::Node {
                kind,
                children: Vec::new(),
            };
            assert!(is_a_comment(&comment), "{kind:?} is a comment");
            assert!(
                !CppGreenNodeBuilder::new().is_layout_of(&comment),
                "{kind:?} is not layout"
            );
        }

        // **A comment written as a bare token is still a comment**, which is the spelling the lexer produces before
        // the documentation layer groups it into a node. Both reach the pass, so both are recognised.
        for kind in [CppTokenKind::LineComment, CppTokenKind::BlockComment] {
            let bare = CppGreenElement::Token {
                kind,
                range: SourceRange::new(0, 4),
            };
            assert!(is_a_comment(&bare), "{kind:?} is a comment");
            assert!(
                !CppGreenNodeBuilder::new().is_layout_of(&bare),
                "{kind:?} is not layout"
            );
        }

        for kind in [
            CppTokenKind::Whitespace,
            CppTokenKind::Newline,
            CppTokenKind::LineContinuation,
        ] {
            let layout = CppGreenElement::Token {
                kind,
                range: SourceRange::new(0, 1),
            };
            assert!(
                CppGreenNodeBuilder::new().is_layout_of(&layout),
                "{kind:?} is layout"
            );
            assert!(!is_a_comment(&layout), "{kind:?} is not a comment");
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
