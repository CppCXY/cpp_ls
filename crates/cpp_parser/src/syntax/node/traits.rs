//! The typed AST layer on top of the untyped rowan tree.
//!
//! # How this layer works
//!
//! The syntax tree is *untyped*: every node knows only its [`CppSyntaxKind`]. This layer adds the
//! types — `CppFunctionDef`, `CppBinaryExpr`, `CppParameter` — as cheap newtypes around a red
//! [`CppSyntaxNode`], with `cast`/`can_cast` deciding whether a node is of that type.
//!
//! That shape (and the names in it) follows the Lua implementation this project grew out of, so code
//! written against the old API keeps working. See `reference/README.md` for where that code lives now.
//!
//! # The three traits
//!
//! * [`CppAstNode`] — a **node** type. Gives typed child/token access and tree navigation.
//! * [`CppAstToken`] — a **token** type. Gives access to the token's text.
//! * [`CppAst`] — a sum type over every node kind, for callers that want to dispatch on what a node
//!   actually is.
//!
//! # Why accessors return `Option`
//!
//! An editor parses files that are being typed, so every construct may be missing, half-written or
//! wrong. An accessor that panicked on a malformed node would turn a typo into a crash, so they
//! return `Option` and callers degrade gracefully. This is also why nothing here validates
//! structure: `get_name()` on a function returns the first name token it can find, whatever the
//! rest of the declaration looks like.

use std::marker::PhantomData;

use rowan::{TextRange, TextSize, WalkEvent};

use crate::kind::{CppKind, CppSyntaxKind, CppTokenKind};
use crate::kind::{CppSyntaxElementChildren, CppSyntaxNodeChildren};

use crate::lexer::CppTokenData;
use crate::syntax::{CppSyntaxId, CppSyntaxNode, CppSyntaxToken};

pub use super::cpp::*;
pub use super::token::*;

/// A typed syntax **node**.
pub trait CppAstNode {
    fn syntax(&self) -> &CppSyntaxNode;

    /// Is a node of this kind a node of type `Self`?
    ///
    /// Separate from `cast` so callers can test a kind without building the wrapper, which is what
    /// [`CppAstChildren`] does on every step of an iteration.
    fn can_cast(kind: CppSyntaxKind) -> bool
    where
        Self: Sized;

    fn cast(syntax: CppSyntaxNode) -> Option<Self>
    where
        Self: Sized;

    /// The first child node of type `N`.
    fn child<N: CppAstNode>(&self) -> Option<N> {
        self.syntax().children().find_map(N::cast)
    }

    /// The first child token of type `N`.
    fn token<N: CppAstToken>(&self) -> Option<N> {
        self.syntax()
            .children_with_tokens()
            .find_map(|it| it.into_token().and_then(N::cast))
    }

    /// The first child token with the given kind, as a generic token.
    fn token_by_kind(&self, kind: CppTokenKind) -> Option<super::token::CppGeneralToken> {
        let token = self
            .syntax()
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .find(|it| it.kind() == CppKind::Token(kind))?;

        super::token::CppGeneralToken::cast(token)
    }

    /// All child nodes of type `N`.
    fn children<N: CppAstNode>(&self) -> CppAstChildren<N> {
        CppAstChildren::new(self.syntax())
    }

    /// All child tokens of type `N`.
    fn tokens<N: CppAstToken>(&self) -> CppAstTokenChildren<N> {
        CppAstTokenChildren::new(self.syntax())
    }

    /// All descendant nodes of type `N`.
    fn descendants<N: CppAstNode>(&self) -> impl Iterator<Item = N> {
        self.syntax().descendants().filter_map(N::cast)
    }

    /// All descendants of type `N`, with enter/leave events.
    fn walk_descendants<N: CppAstNode>(&self) -> impl Iterator<Item = WalkEvent<N>> {
        self.syntax().preorder().filter_map(|event| match event {
            WalkEvent::Enter(node) => N::cast(node).map(WalkEvent::Enter),
            WalkEvent::Leave(node) => N::cast(node).map(WalkEvent::Leave),
        })
    }

    /// All ancestor nodes of type `N`, innermost first.
    fn ancestors<N: CppAstNode>(&self) -> impl Iterator<Item = N> {
        self.syntax().ancestors().filter_map(N::cast)
    }

    /// The root of the tree this node belongs to.
    fn get_root(&self) -> CppSyntaxNode {
        let syntax = self.syntax();
        // `TranslationUnit` is the root, so reaching it means there is nothing above. Using
        // `ancestors().last()` unconditionally would clone the whole chain for the common case.
        if syntax.kind() == CppKind::Syntax(CppSyntaxKind::TranslationUnit) {
            syntax.clone()
        } else {
            syntax.ancestors().last().unwrap_or_else(|| syntax.clone())
        }
    }

    /// The parent node, if it is of type `N`.
    fn get_parent<N: CppAstNode>(&self) -> Option<N> {
        self.syntax().parent().and_then(N::cast)
    }

    fn get_position(&self) -> TextSize {
        self.syntax().text_range().start()
    }

    fn get_range(&self) -> TextRange {
        self.syntax().text_range()
    }

    /// The containing node of type `N`, starting from this node itself.
    fn get_self_or_ancestor<N: CppAstNode>(&self) -> Option<N> {
        N::cast(self.syntax().clone()).or_else(|| self.ancestors().next())
    }

    /// A storable, comparable handle to this node. See [`CppSyntaxId`].
    fn get_syntax_id(&self) -> CppSyntaxId {
        CppSyntaxId::from_node(self.syntax())
    }

    /// The tree, rendered for debugging.
    fn dump(&self) -> String {
        format!("{:#?}", self.syntax())
    }
}

/// An iterator over the child **nodes** of a particular AST type.
#[derive(Debug, Clone)]
pub struct CppAstChildren<N> {
    inner: CppSyntaxNodeChildren,
    ph: PhantomData<N>,
}

impl<N> CppAstChildren<N> {
    pub fn new(parent: &CppSyntaxNode) -> CppAstChildren<N> {
        CppAstChildren {
            inner: parent.children(),
            ph: PhantomData,
        }
    }
}

impl<N: CppAstNode> Iterator for CppAstChildren<N> {
    type Item = N;

    fn next(&mut self) -> Option<N> {
        self.inner.find_map(N::cast)
    }
}

/// A typed syntax **token**.
pub trait CppAstToken {
    fn syntax(&self) -> &CppSyntaxToken;

    fn can_cast(kind: CppTokenKind) -> bool
    where
        Self: Sized;

    fn cast(syntax: CppSyntaxToken) -> Option<Self>
    where
        Self: Sized;

    fn get_token_kind(&self) -> CppTokenKind {
        self.syntax().kind().into()
    }

    /// The token's own data, for callers that want to pass it around without the tree.
    fn get_token_data(&self) -> CppTokenData {
        CppTokenData::new(
            self.get_token_kind(),
            source_range(self.syntax().text_range()),
        )
    }

    fn get_position(&self) -> TextSize {
        self.syntax().text_range().start()
    }

    fn get_range(&self) -> TextRange {
        self.syntax().text_range()
    }

    fn get_syntax_id(&self) -> CppSyntaxId {
        CppSyntaxId::from_token(self.syntax())
    }

    fn get_text(&self) -> &str {
        self.syntax().text()
    }

    fn get_parent<N: CppAstNode>(&self) -> Option<N> {
        self.syntax().parent().and_then(N::cast)
    }

    fn ancestors<N: CppAstNode>(&self) -> impl Iterator<Item = N> {
        self.syntax().parent_ancestors().filter_map(N::cast)
    }

    fn dump(&self) -> String {
        format!("{:#?}", self.syntax())
    }
}

/// An iterator over the child **tokens** of a particular AST type.
#[derive(Debug, Clone)]
pub struct CppAstTokenChildren<N> {
    inner: CppSyntaxElementChildren,
    ph: PhantomData<N>,
}

impl<N> CppAstTokenChildren<N> {
    pub fn new(parent: &CppSyntaxNode) -> CppAstTokenChildren<N> {
        CppAstTokenChildren {
            inner: parent.children_with_tokens(),
            ph: PhantomData,
        }
    }
}

impl<N: CppAstToken> Iterator for CppAstTokenChildren<N> {
    type Item = N;

    fn next(&mut self) -> Option<N> {
        self.inner.find_map(|it| it.into_token().and_then(N::cast))
    }
}

/// Is this node covered by one of `kinds`?
///
/// A helper for the `can_cast` bodies, which are otherwise the same match repeated for every type.
pub fn node_kind_in(kind: CppSyntaxKind, kinds: &[CppSyntaxKind]) -> bool {
    kinds.contains(&kind)
}

/// The token kinds that may appear as trivia, in the order the lexer emits them.
///
/// Exposed because the AST layer frequently has to step over trivia to find the *significant* token
/// next to a construct — "is this declaration followed by a `;`" has to ignore a comment.
pub const TRIVIA_KINDS: &[CppTokenKind] = &[
    CppTokenKind::Whitespace,
    CppTokenKind::Newline,
    CppTokenKind::LineContinuation,
    CppTokenKind::LineComment,
    CppTokenKind::BlockComment,
];

/// Is this token trivia (invisible to the grammar)?
pub fn is_trivia(kind: CppTokenKind) -> bool {
    TRIVIA_KINDS.contains(&kind)
}

/// The significant children of a node, with trivia skipped.
///
/// "Significant" means "not whitespace, newline, line splice or comment" — the tokens the grammar
/// actually sees.
pub fn significant_children(node: &CppSyntaxNode) -> impl Iterator<Item = CppSyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|token| !is_trivia(token.kind().into()))
}

/// The byte range covered by a run of tokens, or `None` for an empty run.
pub fn tokens_range(tokens: &[CppSyntaxToken]) -> Option<TextRange> {
    let first = tokens.first()?.text_range();
    let last = tokens.last()?.text_range();
    Some(TextRange::new(first.start(), last.end()))
}

/// Convert a rowan range into the parser's own range type.
///
/// A free function rather than a `From` impl: `SourceRange` lives in `crate::text` and `TextRange` is
/// rowan's, so the orphan rule forbids implementing the conversion from either side.
pub fn source_range(range: TextRange) -> crate::text::SourceRange {
    crate::text::SourceRange::new(
        u32::from(range.start()) as usize,
        u32::from(range.end() - range.start()) as usize,
    )
}
/// The first child node with one of `kinds`.
///
/// A companion to [`CppAstNode::child`] for the cases where "what kind is this?" is the question
/// being asked, so there is no `N` to infer and `child::<N>()` cannot be used.
pub fn first_child_of_kind(node: &CppSyntaxNode, kinds: &[CppSyntaxKind]) -> Option<CppSyntaxNode> {
    node.children()
        .find(|child| kinds.contains(&CppSyntaxKind::from(child.kind())))
}

/// The first significant token of a node, skipping trivia.
pub fn first_significant_token(node: &CppSyntaxNode) -> Option<CppSyntaxToken> {
    significant_children(node).next()
}

/// The text of a node's **whole subtree**, with trivia removed.
///
/// The token walkers in this module only see *direct* token children, which is the wrong answer for
/// a node whose content is itself a node: `DeclSpecifierSeq` holds a `BuiltinType` holding `int`,
/// so a token-only walk reports nothing at all. This descends instead.
///
/// Whitespace is dropped rather than collapsed, because the callers present these strings as a type
/// spelling — `const int * const` comes out as `constint*const` — so it is for display, and no
/// caller should re-lex the result.
pub fn subtree_text(node: &CppSyntaxNode) -> String {
    let mut out = String::new();
    collect_subtree_text(node, &mut out);
    out
}

fn collect_subtree_text(node: &CppSyntaxNode, out: &mut String) {
    for element in node.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Node(child) => collect_subtree_text(&child, out),
            rowan::NodeOrToken::Token(token) => {
                if !is_trivia(token.kind().into()) {
                    out.push_str(token.text());
                }
            }
        }
    }
}

/// The text of a node's subtree, with whitespace collapsed to single spaces.
///
/// Like [`subtree_text`] but keeps one space between tokens, so `unsigned char` stays readable.
pub fn subtree_text_spaced(node: &CppSyntaxNode) -> String {
    let raw = node.text().to_string();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}
