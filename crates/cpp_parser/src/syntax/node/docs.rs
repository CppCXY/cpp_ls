//! Documentation-comment nodes: the Doxygen layer, typed.
//!
//! A comment's text is not C++, so it is parsed by a second layer of the grammar whose nodes are
//! ordinary children of the same tree — see `grammar::doc`. What that produces is one shape, for
//! every spelling of a comment:
//!
//! ```text
//! DocComment                     one run of consecutive comments: the whole document
//!   DocCommentBody               what the comment says, minus its delimiters
//!     DocCommand                 `@param[in] x the x`
//!       DocCommandName  param    the name, without its `@`
//!       DocCommandArg   [in]     the direction
//!       DocCommandArg   x        the name: for `@param` the *last* argument is the one that names
//!       DocCommandBody  the x    the description
//!     DocCodeBlock               the lines between `@code` and `@endcode`
//! ```
//!
//! # What these wrappers are for
//!
//! Each of the four questions a consumer asks about a comment has a trap in the raw tree, and the
//! accessor here is the answer to it:
//!
//! * **"What is this command called?"** — a `DocCommand`'s text is *not* its name. The arguments and
//!   the body are inside the node, so its text is `param[in] x the x`; [`CppDocCommand::get_name`]
//!   reads the name token instead.
//! * **"What does it document?"** — [`CppDocCommand::get_argument`] is the *last* argument, because
//!   `@param[in] x` has two nodes and the name is the second one. The direction is the first.
//! * **"What does the comment say?"** — the comment's text is not what it says: every `///` line
//!   marker, and the `/**` and `*/` of a block comment, are tokens inside the tree, kept so the file
//!   round-trips byte for byte. [`CppDocComment::get_comment_text`] and
//!   [`CppDocCodeBlock::get_code_text`] read through them.
//! * **"Which lines are code?"** — a code block's contents are deliberately *not* parsed as
//!   documentation, so `get_commands` never reports an `@` inside a snippet.
//!
//! # Nothing here is fallible beyond `Option`
//!
//! The accessors that read a *piece* of a node return `Option`, because an editor parses comments
//! that are half-written: `@param` with no name yet has no argument node, and `@brief` with nothing
//! after it has no body. The accessors that read a node's own text return `String`, because a node
//! that exists always has text — an empty comment yields an empty string, which is what a renderer
//! wants, rather than a `None` it would have to spell the same way.

use crate::{
    CppSyntaxNode, DocCommandKind, command_kind, is_documentation_comment,
    kind::{CppSyntaxKind, CppTokenKind},
    syntax::traits::{CppAstChildren, CppAstNode},
};

use super::{CppDeclaration, CppGeneralToken, CppNameExpr};

// ============================================================================
// The comment
// ============================================================================

/// One run of consecutive comments, parsed as documentation.
///
/// The node a consumer walks. It covers **one or more** adjacent comments — three `///` lines are one
/// document, not three — and it covers ordinary comments too, which are kept for losslessness and
/// contain no commands. Ask [`is_documentation`](Self::is_documentation) to tell the two apart.
///
/// The node has no direct tokens: its opening `///` or `/**` belongs to its [`CppDocCommentBody`]
/// child, which is what makes the body the thing a content walk starts from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocComment {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocComment {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocComment
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocComment {
    /// The body node that wraps the comment's content.
    ///
    /// `None` only for a comment the grammar could not give a body to at all; a comment with no text
    /// still has one, so an empty `///` line is a body with an opener and nothing else.
    pub fn get_body(&self) -> Option<CppDocCommentBody> {
        self.child()
    }

    /// Every command in the comment, in source order.
    ///
    /// A *descendant* walk rather than a `children()` call, and that is the whole point: the commands
    /// are nested inside the comment's body — the body wraps the entire group, so a command on the
    /// third `///` line is a sibling of one on the first — and a consumer asking for "the commands of
    /// this comment" must not have to know that, or to walk a body per line.
    pub fn get_commands(&self) -> impl Iterator<Item = CppDocCommand> {
        self.descendants()
    }

    /// The first command whose name is `name`, in source order.
    ///
    /// Case-sensitive and without the introducer, matching what the tree records: `@param` is `param`.
    /// Case-insensitivity belongs to the command *table* — Doxygen treats `@Param` and `@param` as one
    /// command, and [`CppDocCommand::get_kind`] reports that — but a lookup by name is asking about
    /// the text, so `@Param` is not found by `"param"`. Use
    /// [`get_commands_of_kind`](Self::get_commands_of_kind) to find a command by what it means.
    pub fn get_command(&self, name: &str) -> Option<CppDocCommand> {
        self.get_commands()
            .find(|command| command.get_name().as_deref() == Some(name))
    }

    /// Every command of a given kind, in source order.
    ///
    /// The kind comes from the command table ([`command_kind`]), so it is known for every spelling of
    /// a known command and is `None` for a name the table does not have — a user-defined Doxygen
    /// alias, say, which is a command but not one whose meaning this parser can name.
    pub fn get_commands_of_kind(
        &self,
        kind: DocCommandKind,
    ) -> impl Iterator<Item = CppDocCommand> {
        self.get_commands()
            .filter(move |command| command.get_kind() == Some(kind))
    }

    /// Every item in the comment — command, command body, inline reference, code block — in source
    /// order.
    ///
    /// For a consumer that renders a comment as it was written, rather than querying it by command
    /// name: [`CppDocItem`] is the sum type over the four kinds, in the order they appear, nesting
    /// included.
    pub fn get_items(&self) -> impl Iterator<Item = CppDocItem> {
        self.descendants()
    }

    /// The comment as a human reads it: delimiters and line markers stripped.
    ///
    /// The whole group is rendered, one line per source line, joined with `\n`. Per line:
    ///
    /// * a trailing `*/` goes — the block comment's terminator, which is a token of the comment
    ///   rather than part of what it says;
    /// * a leading marker goes: a run of `/` with an optional `!` (`//`, `///`, `//!`), or an opening
    ///   `/*` with the `*` or `!` after it (`/**`, `/*!`), or a `*` line marker in a block comment;
    /// * one space after the marker goes, and the line's trailing whitespace;
    /// * blank lines at the start and the end go, so the line the opener sits alone on and the line
    ///   the closer sits alone on do not appear; a blank line *between* two lines of text is kept,
    ///   because that is a paragraph break.
    ///
    /// Everything else is returned as written. That includes any indentation past the marker's own
    /// space, and the `@` of a command: this is a *rendering* of the comment, not a parse of it, and
    /// the parsed form is what the node accessors are for.
    pub fn get_comment_text(&self) -> String {
        let text = self.syntax.text().to_string();
        comment_lines(&text, text.trim_start().starts_with("/*")).join("\n")
    }

    /// Is this comment written as documentation (`///`, `//!`, `/**`, `/*!`)?
    ///
    /// The classification is the lexer's, [`is_documentation_comment`], applied to the comment's own
    /// text — so the two layers cannot drift apart about the two spellings that only *look* like
    /// documentation: `////`, which Doxygen reads as a banner, and `/**/` / `/*!*/`, which are empty
    /// block comments and document nothing.
    ///
    /// The *whole* group's text is handed over rather than an opening token, because the empty-block
    /// rule is about the comment as written: `/*!*/` is two tokens, and neither of them alone says
    /// "empty". A group that mixes spellings (`/// doc` followed by `// plain`) is classified by its
    /// first line, which is the opener the question is about.
    pub fn is_documentation(&self) -> bool {
        is_documentation_comment(&self.syntax.text().to_string())
    }

    /// The construct this comment sits in front of.
    ///
    /// A doc comment documents whatever follows it, so the answer is the next node after it in the
    /// tree — `None` at the end of a file or a block, where nothing follows.
    ///
    /// Deliberately not restricted to declarations: a comment can precede a preprocessor directive, a
    /// `template` head, or another comment. [`get_documented_declaration`](Self::get_documented_declaration)
    /// is the narrower question, and it is the one a hover or a signature help wants.
    ///
    /// # Why the *next* node, and not a parent
    ///
    /// The grammar emits a comment into whatever node was open when it was found, which makes the
    /// comment a *sibling* of what it documents rather than a child. That is the right shape for a
    /// lossless tree — a comment between two members belongs to the class body, not to either member —
    /// but it means the relationship has to be read forwards from the comment. There is no back edge
    /// to follow, and inventing one (by re-parenting the comment under its declaration) would put the
    /// comment somewhere the file does not have it.
    pub fn get_owner(&self) -> Option<CppSyntaxNode> {
        next_construct(self.syntax())
    }

    /// The declaration this comment documents.
    ///
    /// Walks forward over the preprocessor directives between a comment and the code it describes —
    /// `/// Doc.` followed by `#define N 3` still documents the declaration after both — to the first
    /// declaration-like node.
    ///
    /// `None` for a comment that documents nothing: a comment at the end of a file or a block, one
    /// whose next construct is a namespace rather than a declaration, or one that is followed by
    /// another comment — a blank line between two comment groups makes them two documents, and the
    /// first one describes the second rather than the declaration behind it. A consumer that wants the
    /// broader relationship should use [`get_owner`](Self::get_owner), which always answers with what
    /// actually follows.
    ///
    /// A class or enum *definition* counts: `/// doc` before `class Grid { ... };` documents the class,
    /// and that is a `Declaration` node carrying a `ClassDef` child.
    pub fn get_documented_declaration(&self) -> Option<CppDeclaration> {
        // A trailing comment documents the declaration *before* it, which is a relationship this
        // forwards walk cannot see. Answering with the declaration after it would attach the comment to
        // the wrong entity — worse than answering nothing — so the marker is recognised and the
        // question is declined.
        if self.is_trailing() {
            return None;
        }

        let mut current = next_construct(self.syntax());

        while let Some(node) = current {
            match CppSyntaxKind::from(node.kind()) {
                CppSyntaxKind::Declaration => return CppDeclaration::cast(node),
                // `using Point = shapes::Point;` is its own node kind rather than a `Declaration`,
                // because its shape is not `specifiers declarators`. It is deliberately *not* added to
                // `CppDeclaration::can_cast`: that would put every `using` into
                // `CppTranslationUnit::get_declarations`, where a consumer would see one spelled
                // "variable" with no name. So a `using` alias is answered here, by the accessor whose
                // question it actually is — "what does this comment document?" — and left out of the
                // general declaration walk.
                CppSyntaxKind::UsingDecl => return CppDeclaration::cast(node),
                // A template head is a wrapper the declaration grammar produces for
                // `template <...> class Grid { ... };`, so a comment in front of one documents the
                // template *declaration* — which is a `Declaration` node like any other.
                CppSyntaxKind::TemplateDecl => return CppDeclaration::cast(node),
                // A directive between the documentation and the code it describes is stepped over.
                CppSyntaxKind::PreprocessorDirective => current = next_construct(&node),
                _ => return None,
            }
        }

        None
    }

    /// Is this a *trailing* comment — `///<` or `//!<`, which documents what comes before it?
    ///
    /// Doxygen's marker, and the reason it matters here is that it inverts the direction of the
    /// relationship: everything else in this file assumes a comment documents what follows it, and a
    /// trailing comment documents what precedes it. The marker is checked on the comment's first line,
    /// because that is where it is written: `///< the x` marks the whole comment.
    pub fn is_trailing(&self) -> bool {
        let text = self.syntax.text().to_string();
        let first = text.lines().next().unwrap_or_default().trim_start();

        first.starts_with("///<")
            || first.starts_with("//!<")
            || first.starts_with("/**<")
            || first.starts_with("/*!<")
    }

    /// The names this comment's [`get_documented_declaration`](Self::get_documented_declaration)
    /// introduces, if it introduces one.
    ///
    /// A convenience over the declaration's own accessor, because the name is not always on the
    /// declaration node: a `using` alias puts it in a `NameExpr` child and leaves the declaration's
    /// own name accessor answering `None`, which is exactly the kind of shape difference a consumer of
    /// comments should not have to know about.
    pub fn get_documented_name(&self) -> Option<String> {
        let declaration = self.get_documented_declaration()?;

        if let Some(name) = declaration.get_name_text() {
            return Some(name);
        }

        // `using Point = shapes::Point;`: the declared name is the `NameExpr` before the `=`, which is
        // the first one — the target is a second `NameExpr` nested inside a `TypeId`.
        super::traits::first_child_of_kind(declaration.syntax(), &[CppSyntaxKind::NameExpr])
            .and_then(CppNameExpr::cast)
            .and_then(|name| name.get_name_token())
            .map(|token| token.get_name_text().to_string())
    }
}

/// The next construct after a node, in the order a reader would reach it.
///
/// "Next" is the following sibling, and when there is none, the next construct after the enclosing
/// node — recursively. The outward step is what makes the answer usable, because the grammar nests a
/// comment wherever it happened to be found: a comment written after a class member is emitted *inside*
/// that member's declaration, and without the step it has no following sibling at all, so a doc comment
/// written just above a member would appear to document nothing.
///
/// Nothing follows the construct at the end of a file, so the walk terminates there. It deliberately
/// does not wrap to the next line or guess.
fn next_construct(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    let mut current = node.clone();

    loop {
        if let Some(sibling) = current
            .siblings_with_tokens(rowan::Direction::Next)
            .skip(1)
            .find_map(|element| element.into_node())
        {
            return Some(sibling);
        }

        // Outward: the remaining siblings are the enclosing node's.
        current = current.parent()?;
    }
}

/// The body of a documentation comment: everything that is not part of a command.
///
/// One of these wraps the whole group's content, so it is also the node that carries the comment's
/// opening delimiter and the line markers between its lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocCommentBody {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocCommentBody {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocCommentBody
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocCommentBody {
    /// The body's own text, trimmed of leading and trailing whitespace.
    ///
    /// This is the *raw* body — delimiters and line markers included — because that is what the node
    /// covers. For the text as a human reads it, use [`CppDocComment::get_comment_text`].
    pub fn get_text(&self) -> String {
        self.syntax.text().to_string().trim().to_string()
    }
}

// ============================================================================
// Commands
// ============================================================================

/// One Doxygen command and everything it owns: `@param[in] x the x`.
///
/// The command's *name* is its first direct token, and the node also contains its argument nodes and
/// its body — so the node's text runs from the name to the end of the payload, and
/// [`get_name`](Self::get_name) is the accessor that reads the name back out. Keeping the arguments
/// and the body inside the node is what lets a consumer ask a command for its payload without pairing
/// it with whichever sibling happens to follow it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocCommand {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocCommand {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocCommand
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocCommand {
    /// The name token: the `param` of `@param`, without its introducer.
    ///
    /// The token kind is [`CppTokenKind::DocCommandName`] rather than `Identifier`, and that is the
    /// reason this accessor is needed at all: inside a comment a command name is not a name that
    /// could be declared, so the grammar records it as a doc token and a consumer walking for
    /// identifiers never sees it.
    pub fn get_name_token(&self) -> Option<CppGeneralToken> {
        self.token_by_kind(CppTokenKind::DocCommandName)
    }

    /// The command's name, without the `@`.
    ///
    /// The name and not the node's text: the node's text is `param[in] x the x`, because the argument
    /// and body nodes are inside it.
    pub fn get_name(&self) -> Option<String> {
        Some(self.get_name_token()?.get_text().to_string())
    }

    /// What the command is for, from the command table.
    ///
    /// `None` for a name the table does not know — a user-defined Doxygen alias, or a misspelling.
    /// That is not an error: the command still parses, with the permissive argument shape, and it is
    /// simply a command whose meaning this parser cannot name.
    pub fn get_kind(&self) -> Option<DocCommandKind> {
        command_kind(&self.get_name()?)
    }

    /// Every argument node the command directly owns, in source order.
    ///
    /// Direct children, not descendants: an argument belongs to the command it was written under, and
    /// a command nested inside a description has arguments of its own.
    pub fn get_args(&self) -> CppAstChildren<CppDocCommandArg> {
        self.children()
    }

    /// The argument that names the thing being documented.
    ///
    /// The **last** argument, because `@param[in] x` has two: the bracketed direction, then the name.
    /// The name is the one a cross-reference resolves — matching `@param x` against a parameter
    /// called `x` is a lookup by this node's text — so "the argument" is the last one, and a caller
    /// that wants the direction asks for it by name ([`CppDocCommandArg::get_direction`]).
    pub fn get_argument(&self) -> Option<CppDocCommandArg> {
        self.get_args().last()
    }

    /// The text of [`get_argument`](Self::get_argument), trimmed.
    ///
    /// `None` when the command has no argument at all — `@param` written with no name yet, which is
    /// the normal state of a line being typed. That is a missing node rather than an empty one, so
    /// "how many parameters does this document?" does not count a parameter that is not there.
    pub fn get_argument_text(&self) -> Option<String> {
        self.get_argument().map(|argument| argument.get_text())
    }

    /// The command's payload: the description it carries.
    pub fn get_body(&self) -> Option<CppDocCommandBody> {
        self.child()
    }

    /// The text of the payload.
    ///
    /// The whole-word split for `@param x desc` is the *grammar's* job, not this accessor's: by the
    /// time there is a tree, `x` is an argument node and `desc` is the body, so this returns `desc`.
    /// Re-splitting the text here would disagree with the tree for the spellings the grammar already
    /// handles — `@param x  desc` with two spaces, or a description continued on the next `///` line.
    pub fn get_body_text(&self) -> Option<String> {
        self.get_body().map(|body| body.get_text())
    }

    /// The code block this command opens, if it is a `@code`-style command.
    ///
    /// Two shapes are tried, because the block is anchored to the *snippet* rather than to the
    /// command: the grammar opens it at the first line of code, which is the line after `@code`, so
    /// in practice the block is the command's next sibling inside the comment body rather than a
    /// child of it. A grammar that nested it under the command would answer the first branch, and
    /// neither shape is assumed.
    pub fn get_code_block(&self) -> Option<CppDocCodeBlock> {
        self.descendants()
            .next()
            .or_else(|| self.syntax().next_sibling().and_then(CppDocCodeBlock::cast))
    }

    /// The code the block covers, as code rather than as comment text.
    ///
    /// See [`CppDocCodeBlock::get_code_text`] — one rule, shared, so the two accessors cannot drift.
    pub fn get_code_text(&self) -> Option<String> {
        Some(self.get_code_block()?.get_code_text())
    }

    /// Is this a callout: `@todo`, `@bug`, `@test`?
    ///
    /// A question about what the command *means*, so an unknown name answers `false` rather than
    /// guessing from its spelling.
    pub fn is_callout(&self) -> bool {
        self.get_kind() == Some(DocCommandKind::Callout)
    }

    /// Does this command describe the documented entity: `@brief`, `@details`, `@return`?
    pub fn is_description(&self) -> bool {
        self.get_kind() == Some(DocCommandKind::Description)
    }
}

/// A command's payload: what the command says, without its own syntax.
///
/// Separate from [`CppDocCommentBody`], which wraps a whole comment's content. This one holds only
/// the description — `the x` of `@param x the x`, `B` of `@brief B` — so a consumer rendering it does
/// not have to strip the command's name or the layout around it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocCommandBody {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocCommandBody {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocCommandBody
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocCommandBody {
    /// The body's text, trimmed of leading and trailing whitespace.
    ///
    /// The grammar already keeps the layout around a body outside it — the space before a `*/`, the
    /// two spaces Doxygen wants after an argument — so the trim is a second fence for the
    /// half-written comments where the tree is still being built. A body may still *end* in a
    /// newline, which is where its line ends and what a consumer joins continuation lines on.
    pub fn get_text(&self) -> String {
        self.syntax.text().to_string().trim().to_string()
    }
}

/// One argument of a command: the `x` of `@param x`, the `[in]` of `@param[in] x`.
///
/// Its own node because it is the part a cross-reference resolves, and because the two kinds of
/// argument are told apart by their *text* rather than by their kind: a direction is a bracketed
/// word, a name is not.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocCommandArg {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocCommandArg {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocCommandArg
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocCommandArg {
    /// The argument as written, trimmed.
    ///
    /// The trim is load-bearing rather than tidy: the grammar records the space that separates an
    /// argument from the description after it *inside the argument's range* — `@param x desc` gives an
    /// argument node whose text is `x ` — so the raw text is not what a cross-reference looks up.
    pub fn get_text(&self) -> String {
        self.syntax.text().to_string().trim().to_string()
    }

    /// Is this a bracketed direction such as `[in]`, `[out]` or `[in,out]`?
    ///
    /// Decided from the text shape, because that is all there is: the grammar records `[`, the word
    /// and `]` as doc tokens with no node of their own, and both a direction (`@param[in]`) and a
    /// parameter range (`@param[1,3]`) are written the same way. An empty bracket is not a direction —
    /// `is_direction` is true exactly when [`get_direction`](Self::get_direction) returns `Some`.
    pub fn is_direction(&self) -> bool {
        self.get_direction().is_some()
    }

    /// The inside of a bracketed direction: `Some("in,out")` for `[in,out]`.
    ///
    /// Returned as written, trimmed — `in` and `out`, not a parsed pair of flags. Splitting on the
    /// comma is Doxygen's own vocabulary and belongs to whichever layer renders the direction, not to
    /// the accessor that found it.
    pub fn get_direction(&self) -> Option<String> {
        let text = self.get_text();
        let inner = text.strip_prefix('[')?.strip_suffix(']')?.trim();
        (!inner.is_empty()).then(|| inner.to_string())
    }
}

// ============================================================================
// Code and inline references
// ============================================================================

/// A code block: the lines between `@code` and `@endcode`.
///
/// Its contents are deliberately *not* parsed as documentation — `std::vector<int>` and `// comment`
/// inside a snippet are code, and reading them as prose turns the snippet into nonsense. What that
/// means for a consumer is the opposite of every other doc node: this one's text is raw, line markers
/// and all, and [`get_code_text`](Self::get_code_text) is the accessor that reads it as code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocCodeBlock {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocCodeBlock {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocCodeBlock
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocCodeBlock {
    /// The code, with the comment's own furniture removed.
    ///
    /// One rule, shared with [`CppDocCommand::get_code_text`]: per line, a trailing `*/` goes, a
    /// leading `/`-run with an optional `!` or an opening `/*` with its `*`/`!` goes, a `*` line
    /// marker goes in a block comment, and one space after the marker goes. Lines are joined with
    /// `\n`, each line's end is trimmed, and the blank lines at the edges are dropped.
    ///
    /// The `*` marker is stripped only inside a block comment. In a `///` comment a leading `*` is
    /// text — and in a code block it is code: `/// *p = 1;` dereferences a pointer, and eating the
    /// `*` would return a snippet that does not compile.
    ///
    /// A line that is exactly the block's `@code` or `@endcode` marker is dropped: the node covers
    /// the closing marker (that is where its range ends), and a marker is not code. A block that was
    /// never closed simply has no such line, which is the normal state of a comment being written.
    pub fn get_code_text(&self) -> String {
        let mut lines = comment_lines(
            &self.syntax.text().to_string(),
            is_in_block_comment(&self.syntax),
        );

        if lines
            .last()
            .is_some_and(|line| is_block_marker(line, "endcode"))
        {
            lines.pop();
        }
        if lines
            .first()
            .is_some_and(|line| is_block_marker(line, "code"))
        {
            lines.remove(0);
        }

        lines.join("\n")
    }
}

/// An inline reference inside a description: `@ref Foo`, `@p name`, `#member`.
///
/// Kept as a node rather than as plain text because it is a link: it names a declaration, and that is
/// what "go to definition" from inside a comment needs.
///
/// The grammar does not produce these yet, so nothing here assumes a shape. The wrapper exists so a
/// consumer can *name* the kind — for a `match` that has to be exhaustive over [`CppDocItem`], or for
/// a walk that reports what a comment contains — and it reports the text the node covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CppDocInline {
    syntax: CppSyntaxNode,
}

impl CppAstNode for CppDocInline {
    fn syntax(&self) -> &CppSyntaxNode {
        &self.syntax
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        kind == CppSyntaxKind::DocInline
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        Self::can_cast(syntax.kind().into()).then_some(Self { syntax })
    }
}

impl CppDocInline {
    /// The reference as written, trimmed.
    pub fn get_text(&self) -> String {
        self.syntax.text().to_string().trim().to_string()
    }
}

// ============================================================================
// The sum type
// ============================================================================

/// Any item inside a documentation comment.
///
/// The four kinds a consumer walking a comment can meet, in the order they nest: a command, the body
/// it carries, an inline reference, and a code block. Dispatch on it when a caller has to render a
/// comment as it was written — where the sequence and the nesting are the point — rather than query
/// it, where [`CppDocComment::get_command`] is the shorter road.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CppDocItem {
    Command(CppDocCommand),
    Body(CppDocCommandBody),
    Inline(CppDocInline),
    CodeBlock(CppDocCodeBlock),
}

impl CppAstNode for CppDocItem {
    fn syntax(&self) -> &CppSyntaxNode {
        match self {
            CppDocItem::Command(node) => node.syntax(),
            CppDocItem::Body(node) => node.syntax(),
            CppDocItem::Inline(node) => node.syntax(),
            CppDocItem::CodeBlock(node) => node.syntax(),
        }
    }

    fn can_cast(kind: CppSyntaxKind) -> bool {
        matches!(
            kind,
            CppSyntaxKind::DocCommand
                | CppSyntaxKind::DocCommandBody
                | CppSyntaxKind::DocInline
                | CppSyntaxKind::DocCodeBlock
        )
    }

    fn cast(syntax: CppSyntaxNode) -> Option<Self> {
        match CppSyntaxKind::from(syntax.kind()) {
            CppSyntaxKind::DocCommand => CppDocCommand::cast(syntax).map(CppDocItem::Command),
            CppSyntaxKind::DocCommandBody => CppDocCommandBody::cast(syntax).map(CppDocItem::Body),
            CppSyntaxKind::DocInline => CppDocInline::cast(syntax).map(CppDocItem::Inline),
            CppSyntaxKind::DocCodeBlock => CppDocCodeBlock::cast(syntax).map(CppDocItem::CodeBlock),
            _ => None,
        }
    }
}

// ============================================================================
// Reading a comment's text
// ============================================================================

/// A comment's lines, each with the comment's own furniture removed.
///
/// Deliberately shared by [`CppDocComment::get_comment_text`] and
/// [`CppDocCodeBlock::get_code_text`]: the two differ in how much of the comment they cover, not in
/// how a line of it is read, and one rule in one place is what keeps `@brief B */` from reporting a
/// brief of `B */` while a snippet reports its code cleanly.
///
/// `block` says whether the text came from a `/* ... */` comment. It is a parameter rather than
/// something re-derived per line because a code block inside a block comment starts at a continuation
/// line — its first character is a `*` marker, not the opener — and asking the fragment would answer
/// "line comment" and leave the markers in the snippet.
fn comment_lines(text: &str, block: bool) -> Vec<String> {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| strip_line_furniture(line, block))
        .collect();

    // The blank lines at the edges are the comment's layout — the line a `/**` sits alone on, the
    // line a `*/` sits alone on — and they go. Trimming the joined result instead would be the easy
    // version of this and the wrong one: it eats the indentation of a code block's first line, which
    // is part of the code.
    while lines.first().is_some_and(|line| line.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    lines
}

/// One line of a comment, with its markers removed. See [`comment_lines`] for the rule.
fn strip_line_furniture(raw: &str, block: bool) -> String {
    // The line's end first, so the closer is the last thing on a line that also has content:
    // `/** @brief B */` has to lose its `*/` before anything else is decided.
    let mut line = raw.trim_end_matches(['\r', ' ', '\t']);
    if block && let Some(rest) = line.strip_suffix("*/") {
        line = rest.trim_end_matches([' ', '\t']);
    }

    // The marker is matched after the line's own indentation, which is a comment's layout rather than
    // its text — `    /// doc` inside a function says the same thing as `/// doc` at column zero.
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let mut rest = &line[indent..];

    // `/*` is tested before the `/`-run, or `/**/` would be read as a run of slashes and come back as
    // a stray `*`: after its closer is stripped it is exactly `/*`, and the answer must be empty.
    if let Some(after) = rest.strip_prefix("/*") {
        // The opener: `/*`, `/**`, `/*!`. Only the `*`/`!` that belong to the marker are eaten.
        rest = after.trim_start_matches(['*', '!']);
    } else if rest.starts_with('/') {
        // A line comment's marker: a run of slashes, plus `!` for the module-documentation spelling.
        // The run stops at the first character that is not a slash, so a `//` *inside* a snippet's
        // line survives — `/// // a comment` is code that says `// a comment`.
        rest = rest.trim_start_matches('/');
        rest = rest.strip_prefix('!').unwrap_or(rest);
    } else if block && let Some(after) = rest.strip_prefix('*') {
        // A `*` line marker on a block comment's continuation line: the ` * text` of
        // `/**\n * text\n */`.
        rest = after;
    }

    // One space after the marker, and only one: what follows it is the text's own indentation, which
    // is what makes an indented code snippet come back indented as it was written.
    rest.strip_prefix(' ')
        .unwrap_or(rest)
        .trim_end_matches([' ', '\t'])
        .to_string()
}

/// Is an already-stripped line one of a code block's own markers?
///
/// Both introducers are accepted, because Doxygen spells every command with `@` or `\` and people mix
/// them: a block opened with `@code` is regularly closed with `\endcode`.
fn is_block_marker(line: &str, marker: &str) -> bool {
    let line = line.trim();
    line.strip_prefix('@').or_else(|| line.strip_prefix('\\')) == Some(marker)
}

/// Is the comment this node belongs to written as a block comment?
///
/// Asked of the enclosing `DocComment` rather than of the node's own text, because a code block's
/// first line is a continuation line — its marker is a `*`, not the opener — so the fragment cannot
/// answer the question. A node outside any comment answers `false`: a `*` is then more likely to be
/// text than a marker, and leaving a byte in is the safe direction.
fn is_in_block_comment(node: &CppSyntaxNode) -> bool {
    node.ancestors()
        .find(|it| CppSyntaxKind::from(it.kind()) == CppSyntaxKind::DocComment)
        .is_some_and(|comment| comment.text().to_string().trim_start().starts_with("/*"))
}
