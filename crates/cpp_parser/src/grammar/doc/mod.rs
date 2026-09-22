//! The Doxygen comment grammar: the second layer of the parser.
//!
//! # Why there is a second layer
//!
//! A comment's text is not C++, and a C++ grammar has no business reading it. But it is also not
//! *nothing*: an editor that cannot say "this declaration is documented, its `@param` documents `x`,
//! and that word is a link to `Foo`" cannot offer hover text, signature help or go-to-definition
//! from a comment. So the comment is re-parsed, by a parser that knows only comments, and its result
//! is grafted into the same tree.
//!
//! # How the two layers meet
//!
//! The comment tokens never reach the C++ tree as tokens. Instead [`parse_comment_group`] emits its
//! nodes into the *same* event stream the C++ grammar is writing, through the same
//! [`MarkerEventContainer`] the C++ parser implements. There is no second tree and no copying: the
//! doc nodes are ordinary children of whatever node was open when the comment was found.
//!
//! That works because a comment is a contiguous span of the file and every doc token carries a range
//! in **file coordinates** — see [`crate::lexer::lex_comment`]. Nothing has to be translated, and a
//! consumer walking the tree finds `@param` nodes whose ranges answer "where in the file?" directly.
//!
//! # Recovery
//!
//! Every rule here is total. A comment is whatever the user has typed so far, so there is no such
//! thing as a comment that is too broken to represent — the fallback is always "record it as text".
//! That is what makes the layer safe to run on every keystroke, and it is the same contract the C++
//! layer has: never panic, never drop a byte, never stop.

mod commands;

use crate::{
    grammar::ParseResult,
    kind::CppSyntaxKind,
    lexer::{DocToken, DocTokenKind, is_block_comment, is_documentation_comment, lex_comment},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
    text::SourceRange,
};

pub use commands::{
    DocCommandArgs, DocCommandKind, END_CODE_COMMAND, UNKNOWN_COMMAND_ARGS, lookup,
};

/// What a command is for, or `None` when the name is not one this parser knows.
///
/// Exposed for consumers that want to group a comment's commands — "list the parameters" is
/// `kind == Some(Parameter)` — without depending on the table's representation.
pub fn command_kind(name: &str) -> Option<DocCommandKind> {
    lookup(name).map(|spec| spec.kind)
}

/// The argument shape a command's name implies, or [`UNKNOWN_COMMAND_ARGS`] for a name that is not
/// in the table.
pub fn command_args(name: &str) -> DocCommandArgs {
    lookup(name).map_or(UNKNOWN_COMMAND_ARGS, |spec| spec.args)
}

/// One comment to parse: where it is, and what separates it from the previous one.
///
/// Deliberately *not* a borrow of the parser's text. The doc parser needs `&mut` on the parser to
/// emit its events, so a source holding `&parser.text` would make the two borrows overlap — and
/// resolving the text at the point of use costs nothing, because the file is already in hand there.
#[derive(Debug, Clone, Copy)]
pub struct CommentSource {
    /// The comment's range in the **original file**.
    pub range: SourceRange,
    /// The trivia between this comment and the previous one in its group: the newline, and any
    /// indentation. Empty for the first comment.
    ///
    /// Carried *into* the parser rather than emitted by the caller, because the separator has to end
    /// up inside the `DocComment` node. A node whose text is the concatenation of its tokens must not
    /// have holes, or `node.text()` stops matching the node's range — and then every consumer that
    /// reads a comment's text reads it wrongly, including the API a caller uses to show hover text.
    pub separator: [Option<SourceRange>; MAX_SEPARATOR_TOKENS],
    /// How many of `separator` are real. Between two comments there is a newline, sometimes
    /// indentation, and never much else, so the array is bounded and no allocation is needed.
    pub separator_len: usize,
}

/// The most layout tokens there can be between two comments of one group.
///
/// A newline plus indentation is the normal case. The bound exists so a `CommentSource` can be `Copy`
/// and allocation-free; a group whose gap has more layout than this is refused by the grouping, which
/// is the honest answer — a gap that wide is not "adjacent comments".
pub const MAX_SEPARATOR_TOKENS: usize = 4;

impl CommentSource {
    /// A comment with nothing before it.
    pub fn first(range: SourceRange) -> Self {
        CommentSource {
            range,
            separator: [None; MAX_SEPARATOR_TOKENS],
            separator_len: 0,
        }
    }

    /// The separator's tokens, in order.
    pub fn separator_tokens(&self) -> impl Iterator<Item = SourceRange> + '_ {
        self.separator[..self.separator_len]
            .iter()
            .filter_map(|range| *range)
    }

    /// The end of the comment in the original file.
    ///
    /// The doc parser is bounded by this, and the bound is load-bearing rather than tidy: a comment's
    /// tokens must tile exactly the comment and nothing beyond it. If a rule were allowed to read one
    /// token further it would consume the newline that separates two comments of a group — and that
    /// newline belongs to the group, so the byte would appear twice in the tree.
    pub fn end(&self) -> usize {
        self.range.end_offset()
    }
}

/// Parse a run of consecutive comments as one documentation block.
///
/// The run becomes a single [`CppSyntaxKind::DocComment`] node whose children are the parsed
/// commands, in order. One node for the run rather than one per comment, because a document is
/// written across several `///` lines and splitting it would push the regrouping onto every consumer.
///
/// `comments` is never empty; the caller only calls this when it has at least one.
pub fn parse_comment_group(p: &mut CppParser<'_>, comments: &[CommentSource]) -> ParseResult {
    let m = p.mark(CppSyntaxKind::DocComment);

    // Doc token ranges are offsets into the file, so this is what they are sliced with.
    let file = p.origin_text();

    // `@code` spans comments: it opens on one `///` line and closes several lines later, and each of
    // those lines is a separate comment as far as the C++ lexer is concerned. So both the code block's
    // depth and the marker of the node being built for it belong to the *group*.
    //
    // The marker is what makes the block's node span the snippet. `@code` is seen in one comment and
    // `@endcode` in another, and the node has to cover everything between them — opening it at the
    // `@endcode` would produce a node holding the word `endcode` and none of the code, which is
    // worse than useless to a consumer that wants to render the snippet.
    let mut code_block_depth = 0usize;
    let mut open_code_block: Option<crate::parser::Marker> = None;

    // **One body for the whole group**, not one per comment.
    //
    // A documentation block is written across several `///` lines, and a consumer asking "what does
    // this declaration's documentation say?" wants the commands in order — not a body per line with
    // the commands scattered one level deeper in each. `DocComment::get_commands` is the accessor, and
    // it only works if the body is the group.
    //
    // It is opened before the loop and closed after it, which is also what lets a construct spanning
    // comments — `@code`, whose snippet is on the lines between `@code` and `@endcode` — sit inside
    // the body rather than being split across one body per line.
    let body = p.mark(CppSyntaxKind::DocCommentBody);

    for comment in comments {
        // The separator before this comment is part of the group's text, so it goes in first. A
        // newline is recorded as a newline and anything else as whitespace — which is all the layout
        // between two comments of a group can be.
        for range in comment.separator_tokens() {
            let kind = if file[range.start_offset..range.end_offset()].contains('\n') {
                crate::kind::CppTokenKind::Newline
            } else {
                crate::kind::CppTokenKind::Whitespace
            };
            p.push_doc_token(kind, range);
        }

        let text = &file[comment.range.start_offset..comment.range.end_offset()];
        let tokens = lex_comment(text, comment.range.start_offset);
        let documentation = is_documentation_comment(text);
        let is_block = is_block_comment(text);

        let mut parser = DocParser::new(
            p,
            tokens,
            file,
            is_block,
            comment.end(),
            code_block_depth,
            open_code_block,
        );
        if documentation {
            parser.parse_document();
        } else {
            // An ordinary comment is still a `DocComment` node — see the kind's documentation — but
            // nothing in it is documentation, so it is recorded as text and not parsed for commands.
            // That keeps `// @param x` in a normal comment from being mistaken for documentation
            // while still losing none of its bytes.
            parser.parse_plain();
        }
        code_block_depth = parser.code_block_depth;
        open_code_block = parser.open_code_block;
    }

    body.complete(p);

    // A block that never closed is the normal state of a comment being written. Close it after the
    // body so it cannot swallow whatever follows the group.
    if let Some(block) = open_code_block {
        block.complete(p);
    }

    Ok(m.complete(p))
}

/// The second-layer parser.
///
/// It shares the event stream and the marker stack with [`CppParser`] rather than owning a tree of
/// its own, which is what makes the doc nodes ordinary nodes of the C++ tree. Everything it needs
/// beyond that is a token list and a cursor.
struct DocParser<'a, 'p> {
    p: &'a mut CppParser<'p>,
    /// The comment's doc tokens.
    ///
    /// Owned rather than borrowed because `emit_argument` splits a text run in two when an argument is
    /// followed by its description on the same line — `@param x desc` — and the remainder has to become
    /// a token of its own. One comment's token list is small and short-lived, so owning it costs
    /// nothing measurable.
    tokens: Vec<DocToken>,
    /// The **whole file**, which is what doc token ranges are offsets into.
    ///
    /// Storing the file rather than the comment is not a convenience: every range in this layer is in
    /// file coordinates — that is what lets the events be appended to the C++ stream with no
    /// translation — so slicing with them needs the file. The comment's own text is only ever needed
    /// to *classify* it, and that is decided before the parser is built.
    text: &'a str,
    index: usize,
    /// How deep inside `@code ... @endcode` the parser is.
    ///
    /// A counter rather than a flag, and owned by the *group* rather than by one comment: `@code`
    /// opens on one `///` line and closes several lines later, and each line is a separate comment as
    /// far as the C++ lexer is concerned. A flag reset between comments would let the code inside a
    /// block be read as documentation — which is exactly what `@code` exists to prevent.
    code_block_depth: usize,
    /// Is this a block comment? Its body continues across newlines; a line comment's does not.
    is_block: bool,
    /// The comment's end in the file. The parser never reads past it.
    end_offset: usize,
    /// The `DocCodeBlock` node currently being built, if any.
    ///
    /// Carried across comments because `@code` and `@endcode` are usually on different `///` lines,
    /// and the node has to span everything between them. Opening it at the `@endcode` would give a
    /// consumer a "code block" holding the word `endcode` and none of the code.
    open_code_block: Option<crate::parser::Marker>,
}

impl<'a, 'p> DocParser<'a, 'p> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        p: &'a mut CppParser<'p>,
        tokens: Vec<DocToken>,
        text: &'a str,
        is_block: bool,
        end_offset: usize,
        code_block_depth: usize,
        open_code_block: Option<crate::parser::Marker>,
    ) -> Self {
        DocParser {
            p,
            tokens,
            text,
            index: 0,
            code_block_depth,
            is_block,
            end_offset,
            open_code_block,
        }
    }

    /// Is the cursor at or past the end of this comment?
    ///
    /// The token list already stops at the comment, so this is a second fence in the same place. It
    /// exists because `parse_body` decides whether to continue past a newline, and at the comment's
    /// edge that answer must be "no" whether or not the list happens to have more in it — the newline
    /// beyond the edge is the trivia *between* two comments and belongs to neither.
    fn past_comment_end(&self) -> bool {
        self.current_range().start_offset >= self.end_offset
    }

    // ========================================================================
    // Token access
    // ========================================================================

    fn current(&self) -> DocTokenKind {
        self.tokens
            .get(self.index)
            .map_or(DocTokenKind::Eof, |token| token.kind)
    }

    fn current_range(&self) -> SourceRange {
        self.tokens
            .get(self.index)
            .map_or(SourceRange::EMPTY, |token| token.range)
    }

    fn current_text(&self) -> &'a str {
        match self.tokens.get(self.index) {
            Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
            None => "",
        }
    }

    fn at_end(&self) -> bool {
        self.index >= self.tokens.len()
    }

    /// Emit the current token into the shared stream and advance.
    fn bump(&mut self) {
        if let Some(token) = self.tokens.get(self.index) {
            self.p.push_token_event(token);
        }
        self.index += 1;
    }

    /// Advance without emitting. Used for the tokens a construct deliberately discards — the `*` of a
    /// block-comment continuation, which the lexer already folded into whitespace, is the main one.
    fn skip_layout(&mut self) {
        while self.current().is_trivia() {
            self.bump();
        }
    }

    /// Emit every token up to, and including, the end of the current line.
    ///
    /// The terminator is part of the line rather than a separator after it, and including it is what
    /// keeps a construct that spans several lines — `@code`, whose node is opened on one `///` line
    /// and closed on another — from being *empty* at the moment it is opened. A node with no children
    /// is dropped as carrying no information, so a code block opened without its newline would
    /// disappear from the tree and take its snippet with it.
    fn bump_to_end_of_line(&mut self) {
        while !self.at_end() && !self.past_comment_end() {
            let ends_line = self.current().ends_a_line();
            self.bump();
            if ends_line {
                break;
            }
        }
    }

    // ========================================================================
    // The document
    // ========================================================================

    /// Parse a documentation comment: a sequence of commands, with any text before the first command
    /// being the document's summary.
    ///
    /// The first branch is what makes `@code` work across the several `///` lines a block is written
    /// on. Inside a block, nothing is a command — not `@notacommand`, not even `@param` — and the only
    /// thing that ends it is `@endcode`. Dispatching on the depth *before* looking at the token is
    /// what enforces that; checking it inside `parse_command` would be too late, because by then a
    /// node has been opened for a command that is really code.
    fn parse_document(&mut self) {
        while !self.at_end() {
            if self.code_block_depth > 0 {
                self.parse_code_block();
                continue;
            }

            self.skip_layout();

            if self.at_end() {
                break;
            }

            match self.current() {
                DocTokenKind::DocCommandName => self.parse_command(),
                // Text before the first command is the brief description. Doxygen's own rule: a
                // comment's first paragraph is the brief, whether or not `@brief` was written. A doc
                // comment that only ever says `@param` never reaches this.
                DocTokenKind::DocText => self.parse_free_description(),
                // Punctuation at the start of a line, or a stray end marker: nothing to build, but it
                // must still be emitted so the tree keeps the byte.
                _ => self.bump(),
            }
        }
    }

    /// A comment that is not documentation: record its text, look for nothing.
    fn parse_plain(&mut self) {
        while !self.at_end() {
            self.bump();
        }
    }

    /// The running text before any command.
    fn parse_free_description(&mut self) {
        let m = self.p.mark(CppSyntaxKind::DocCommandBody);
        while !self.at_end() && !self.starts_a_command_here() && !self.body_ends_here() {
            self.bump();
        }
        m.complete(self.p);
    }

    // ========================================================================
    // Commands
    // ========================================================================

    /// One command: its name, its arguments, and its body.
    ///
    /// The command's *shape* comes from [`commands::lookup`], which is what keeps this one function
    /// rather than one per command. A name that is not in the table is not an error — Doxygen has
    /// user-defined aliases — so it gets the permissive shape: keep the rest of the line as a body.
    fn parse_command(&mut self) {
        let m = self.p.mark(CppSyntaxKind::DocCommand);

        let name = self.current_text().to_string();
        let spec = commands::lookup(&name);
        self.bump(); // the name itself

        // Inside `@code`, a command is code. The block's *end* is handled by `parse_document`, which
        // checks for `@endcode` before dispatching anything as a command, so this branch is only ever
        // reached for a `@` that is part of a snippet.
        if self.code_block_depth > 0 {
            self.bump_to_end_of_line();
            m.complete(self.p);
            return;
        }

        let args = spec.map_or(UNKNOWN_COMMAND_ARGS, |spec| spec.args);

        match args {
            DocCommandArgs::None => {
                self.bump_to_end_of_line();
            }
            DocCommandArgs::Body => {
                self.parse_body();
            }
            DocCommandArgs::NameAndBody => {
                self.skip_layout();
                self.parse_name_argument();
                self.parse_body();
            }
            DocCommandArgs::DirectionalNameAndBody => {
                self.skip_layout();
                self.parse_direction_argument();
                self.skip_layout();
                self.parse_name_argument();
                self.parse_body();
            }
            DocCommandArgs::ReferenceAndBody => {
                self.skip_layout();
                self.parse_reference_argument();
                self.parse_body();
            }
            DocCommandArgs::SectionTitle => {
                self.skip_layout();
                self.parse_name_argument();
                self.parse_body();
            }
            DocCommandArgs::CodeBlock => {
                // Nothing is emitted here. `parse_code_block` owns every token of the block, including
                // the line `@code` is on and the `@endcode` that closes it — emitting any of them here
                // as well would put the byte in the tree twice and split the node.
                self.code_block_depth += 1;
            }
        }

        // The node's text therefore runs from the command's name to the end of its body: `brief B`.
        // That is a property of the range, not of the name — the name is the node's first token, and
        // reading it as text is what the accessor layer does. Shortening the range here to make the
        // text be the name alone would silently drop the token that carried the rest of the bytes.
        m.complete(self.p);
    }

    /// The whole rest of the line, or to the end of a block comment, as the command's payload.
    ///
    /// The node's text is what the command *says* — `B`, not ` B`, not `B `, and never `B */`. Layout
    /// that surrounds it belongs to the comment or to the command, and is emitted outside this node
    /// rather than moved out of it afterwards: moving bytes out of a node means emitting them a second
    /// time somewhere else, and the window in which both copies exist is where a losslessness bug lives.
    ///
    /// Never reads past the comment. A token beyond the comment is trivia between two comments of the
    /// group, and it is emitted by the group loop — consuming it here would put the byte in the tree
    /// twice.
    fn parse_body(&mut self) {
        // The separator before the body is the command's, not the description's: the lexer usually
        // hands it over as its own token, but text that follows an argument on the same line arrives
        // attached to the first word — `@param x the x` gives `" the x"` — so both spellings are
        // handled here.
        self.skip_layout();
        self.split_leading_layout();

        let m = self.p.mark(CppSyntaxKind::DocCommandBody);

        while !self.at_end() && !self.past_comment_end() {
            match self.current() {
                // A new command ends this body, and the introducer is left outside: `@brief B\n * @param
                // x` must not report a brief of `B\n * @`. Stopping *before* the `@` is what keeps the
                // `\n * ` out of the body.
                DocTokenKind::DocIntroducer | DocTokenKind::DocCommandName
                    if self.starts_a_command_here() =>
                {
                    break;
                }
                // The comment's own terminator ends it too, and is left outside.
                DocTokenKind::BlockCommentEnd => break,
                // A newline ends the *line* being read but not the body of a block comment: `@brief one`
                // and the `* two` under it are one brief.
                DocTokenKind::DocNewline => {
                    self.bump();
                    if !self.block_comment_continues() {
                        break;
                    }
                }
                // Layout with nothing after it belongs to the comment, not to the command.
                _ if self.body_ends_here() => break,
                _ => self.bump(),
            }
        }

        m.complete(self.p);
    }

    /// Split the leading blanks off the text token at the cursor, and emit them.
    ///
    /// A text run can begin with the whitespace that separates it from whatever came before — after an
    /// argument, the description of `@param x the x` is handed over as `" the x"`. That space is the
    /// command's layout, not the description's first character, so it is emitted as a token of its own
    /// and the text token is shortened to what is left.
    fn split_leading_layout(&mut self) {
        let Some(token) = self.tokens.get(self.index) else {
            return;
        };
        if token.kind != DocTokenKind::DocText {
            return;
        }

        let range = token.range;
        let Some(slice) = self.text.get(range.start_offset..range.end_offset()) else {
            return;
        };
        let layout = slice.len() - slice.trim_start_matches([' ', '\t']).len();
        if layout == 0 || layout == slice.len() {
            return;
        }

        self.tokens[self.index].range =
            SourceRange::new(range.start_offset + layout, range.length - layout);
        self.tokens.insert(
            self.index,
            DocToken::new(
                DocTokenKind::DocWhitespace,
                SourceRange::new(range.start_offset, layout),
            ),
        );
        self.skip_layout();
    }

    /// Does the body end at the cursor — is everything from here on the comment's own layout?
    ///
    /// True when nothing but blanks separate the cursor from the comment's closer or from the next
    /// command. This is what keeps a one-line body from swallowing the space in `/** @brief B */`, and a
    /// block body from swallowing the `\n * ` before the line `*/` is on.
    fn body_ends_here(&self) -> bool {
        let next = self.tokens[self.index..]
            .iter()
            .position(|token| !token.kind.is_trivia())
            .map(|offset| self.index + offset);

        match next {
            // The comment is over: everything left is its layout.
            Some(index) => {
                self.tokens[index].kind == DocTokenKind::BlockCommentEnd
                    || self.command_starts_at(index)
            }
            // Nothing left but layout, and no comment end to reach: the group's last comment stops at
            // its own last token, so there is no content after it by definition.
            None => true,
        }
    }

    /// Is a command starting at the cursor?
    ///
    /// An introducer on its own is not a command — `user@example.com` and a trailing `@` are prose —
    /// so this needs the name too. The name is the *next* token because a newline between them is
    /// possible (`@\nparam` is not valid, but the lexer does not know that) and because testing only
    /// for `DocIntroducer` is what let `@brief B\n * @param x` report a brief of `B\n * @`.
    fn starts_a_command_here(&self) -> bool {
        match self.current() {
            DocTokenKind::DocCommandName => true,
            DocTokenKind::DocIntroducer => self.tokens.get(self.index + 1).is_some_and(|token| {
                matches!(
                    token.kind,
                    DocTokenKind::DocCommandName | DocTokenKind::DocWhitespace
                )
            }),
            _ => false,
        }
    }

    /// Does a command start at `index`, layout to one side?
    ///
    /// The lexer hands over `@brief` as a single name token, but `@ brief` as an introducer plus a
    /// space plus a name — both spellings mean the same thing, and a body has to stop before either.
    /// One space is allowed between them, no more, and never a newline: a line ending in a bare `@` is
    /// prose, and the next line's first word is not its command.
    fn command_starts_at(&self, index: usize) -> bool {
        match self.tokens.get(index).map(|token| token.kind) {
            Some(DocTokenKind::DocCommandName) => true,
            Some(DocTokenKind::DocIntroducer) => {
                let rest = &self.tokens[index + 1..];
                // `take_while` can only stop at the first token that is not a space, so the token it
                // stopped at is the name — or the newline that ends the search.
                let spaces = rest
                    .iter()
                    .take_while(|token| token.kind == DocTokenKind::DocWhitespace)
                    .count();
                spaces <= 1
                    && rest[spaces..]
                        .first()
                        .is_some_and(|token| token.kind == DocTokenKind::DocCommandName)
            }
            _ => false,
        }
    }

    /// A newline ends the *line* being read but not the body of a block comment: `@brief one\n * two`
    /// is one brief. A line comment's `///` lines are separate comments, and the next one starts its
    /// own text.
    fn block_comment_continues(&self) -> bool {
        !self.at_end() && !self.past_comment_end() && self.is_block
    }

    /// The contents of `@code ... @endcode`: emit, do not interpret.
    ///
    /// A code block is full of `@`, `<` and `//`, and reading it as documentation turns a snippet into
    /// nonsense — `std::vector<int>` would become a reference to `std` with template arguments, and
    /// `// @notacommand` inside the snippet would become a command.
    ///
    /// A block spans comments: `@code` is on one `///` line, the snippet on the next few, `@endcode` on
    /// another. Each of those lines is a separate comment with its own parser, so the block's marker
    /// and its depth belong to the group. Which is why the ordering below is the whole point of this
    /// function and is not duplicated anywhere else:
    ///
    /// 1. emit the tokens of the line being read, interpreting none of them;
    /// 2. at the `@endcode`, emit *that* token;
    /// 3. only then close the node.
    ///
    /// Emitting `@endcode` anywhere else — in the command handler, say, which is where a
    /// command-shaped token naturally goes — emits it twice and puts the `NodeEnd` in the wrong place.
    /// The symptom is a block covering one comment with the rest of the documentation nested inside
    /// it, while every byte is still present and the tree is still well formed.
    fn parse_code_block(&mut self) {
        while !self.at_end() && !self.past_comment_end() {
            // The node is opened by the first token that belongs to it, and *only* where there is one.
            //
            // `@code` is normally alone on its `///` line, so the block's first real token is on the
            // next line — which is a different comment, parsed by a different call to this function.
            // Opening the node eagerly, from the `@code` line, produces a node whose only content is
            // its own `NodeStart`: an empty node is dropped as carrying no information, and the marker
            // goes with it, leaving the `NodeEnd` emitted at the `@endcode` to close an unrelated node.
            if self.open_code_block.is_none() {
                self.open_code_block = Some(self.p.mark(CppSyntaxKind::DocCodeBlock));
            }

            // `@endcode` is recognised by its *name*, and the name is one token past the introducer.
            // Testing for `DocCommandName` alone would never fire: the loop sees the `@` first.
            if self.ends_the_code_block() {
                if self.current() == DocTokenKind::DocIntroducer {
                    // `@endcode` written with no space: the introducer is still pending.
                    self.bump();
                }
                self.bump(); // `endcode`
                self.bump_to_end_of_line();
                self.code_block_depth -= 1;

                if let Some(block) = self.open_code_block.take() {
                    block.complete(self.p);
                }
                return;
            }
            self.bump();
        }
    }

    /// Is the cursor on the `@endcode` that closes this block?
    ///
    /// Recognised by *name*, and the name is one token past the introducer: `skip_layout` has already
    /// emitted the `@` by the time this is asked, so only the `DocCommandName` arm fires in practice.
    /// The introducer arm is kept for the compact spelling `@endcode` with no space, where the two
    /// tokens are still adjacent.
    fn ends_the_code_block(&self) -> bool {
        match self.current() {
            DocTokenKind::DocCommandName => {
                self.current_text().eq_ignore_ascii_case(END_CODE_COMMAND)
            }
            DocTokenKind::DocIntroducer => self.tokens.get(self.index + 1).is_some_and(|token| {
                token.kind == DocTokenKind::DocCommandName
                    && self.text[token.range.start_offset..token.range.end_offset()]
                        .eq_ignore_ascii_case(END_CODE_COMMAND)
            }),
            _ => false,
        }
    }

    /// The name of the thing being documented: the parameter of `@param x`.
    ///
    /// Recorded as its own node because it is the part a cross-reference resolves: matching `@param x`
    /// against the parameter named `x` is a lookup by this node's text, and doing that from a token
    /// stream means re-implementing the argument grammar at every use.
    fn parse_name_argument(&mut self) {
        if !self.starts_an_argument() {
            return;
        }

        let m = self.p.mark(CppSyntaxKind::DocCommandArg);
        self.emit_argument();
        m.complete(self.p);
    }

    /// The `[in]`, `[out]` or `[1,3]` of `@param`.
    ///
    /// Doxygen allows both a direction and a range in one bracket — `@param[in,out]` and
    /// `@param[1,3]` — and they are distinguished by whether the contents are digits. Either way the
    /// whole bracket is one argument node, because that is how the standard presents it.
    fn parse_direction_argument(&mut self) {
        if self.current() != DocTokenKind::DocLeftBracket {
            return;
        }

        let m = self.p.mark(CppSyntaxKind::DocCommandArg);
        self.bump(); // `[`

        while !self.at_end()
            && !matches!(
                self.current(),
                DocTokenKind::DocRightBracket | DocTokenKind::DocNewline
            )
        {
            self.bump();
        }

        if self.current() == DocTokenKind::DocRightBracket {
            self.bump();
        }

        m.complete(self.p);
    }

    /// The target of `@ref`: a name, possibly qualified and possibly with template arguments.
    ///
    /// Read as text rather than parsed as a C++ name. Inside a comment there is no type-name table
    /// and no scope to resolve against, so the honest thing is to record what was written and let the
    /// layer that resolves references decide what it means.
    fn parse_reference_argument(&mut self) {
        if !self.starts_an_argument() {
            return;
        }

        let m = self.p.mark(CppSyntaxKind::DocCommandArg);
        self.emit_argument();
        m.complete(self.p);
    }

    // ========================================================================
    // Arguments
    // ========================================================================

    /// Is there an argument at the cursor?
    ///
    /// An argument is a run of tokens up to the next whitespace, newline or command — so `@param x
    /// desc` has the argument `x` and the body ` desc`, not one long argument. That split is the
    /// whole point of parsing `@param`: the name is the part a cross-reference resolves, and a name
    /// with a description glued to it resolves to nothing.
    fn starts_an_argument(&self) -> bool {
        matches!(
            self.current(),
            DocTokenKind::DocText
                | DocTokenKind::DocScope
                | DocTokenKind::DocDot
                | DocTokenKind::DocColon
                | DocTokenKind::DocLess
                | DocTokenKind::DocGreater
                | DocTokenKind::DocEquals
        )
    }

    /// Emit one argument's tokens and advance past it.
    ///
    /// An argument is a single word, so it ends at the first space that has something after it —
    /// `@param x desc` documents `x`, and the `desc` is the body. Without that split the argument node
    /// would contain the whole phrase, and a cross-reference resolving `@param x` would look up a
    /// parameter literally named `x desc`.
    ///
    /// The split is done here rather than in the lexer because only the parser knows that it is
    /// reading an argument: after `@brief`, a space is just a space, and splitting there would cut
    /// every description into words.
    ///
    /// Assembling the argument from tokens rather than slicing the source directly matters because
    /// the tokens are what get emitted: the argument node must contain exactly the tokens whose text
    /// it reports, or the node's range and the text a consumer reads from it would disagree.
    fn emit_argument(&mut self) {
        if !self.starts_an_argument() {
            return;
        }

        let Some(token) = self.tokens.get(self.index) else {
            return;
        };
        let range = token.range;

        if token.kind == DocTokenKind::DocText
            && let Some(space) = self.text[range.start_offset..range.end_offset()].find(' ')
            && space > 0
            && space + 1 < range.length
        {
            // Truncate *before* emitting. `bump` reads the token out of `self.tokens` to build the
            // event, so shortening it afterwards would emit the whole run and leave the argument node
            // spanning the description it was supposed to stop before — while the token inside it said
            // something different, which is the kind of disagreement between a node's range and its
            // text that makes every consumer read the wrong thing.
            let split = range.start_offset + space;
            self.tokens[self.index].range.length = space;
            self.bump();

            // The space itself becomes a token of the *command*, not of the description that follows.
            // A body's text is what the description says, and a consumer rendering it must not get the
            // separator the command's own syntax implies.
            self.tokens.insert(
                self.index,
                DocToken::new(DocTokenKind::DocWhitespace, SourceRange::new(split, 1)),
            );
            self.bump();

            // The rest becomes a text token of its own, so the body that follows covers exactly the
            // bytes the argument does not.
            let remainder = DocToken::new(
                DocTokenKind::DocText,
                SourceRange::new(split + 1, range.end_offset() - split - 1),
            );
            self.tokens.insert(self.index, remainder);
            return;
        }

        while let Some(token) = self.tokens.get(self.index) {
            match token.kind {
                DocTokenKind::DocWhitespace
                | DocTokenKind::DocNewline
                | DocTokenKind::DocCommandName
                | DocTokenKind::Eof => break,
                _ => {
                    self.bump();
                    // A second run can only be there because the argument was written with a space in
                    // it, which no parameter name is.
                    if matches!(
                        self.current(),
                        DocTokenKind::DocText | DocTokenKind::DocWhitespace
                    ) {
                        break;
                    }
                }
            }
        }
    }
}

/// Marker bookkeeping for the doc layer.
///
/// The doc parser emits into the C++ parser's stream, so its markers are the C++ parser's markers.
/// This is a free function rather than a second trait implementation because there is nothing to
/// adapt: the doc nodes go on the same stack as everything else, which is exactly what makes them
/// ordinary children of the C++ tree.
impl CppParser<'_> {
    /// Push an already-lexed doc token into the event stream.
    pub(crate) fn push_token_event(&mut self, token: &DocToken) {
        self.push_doc_token(token.kind.to_cpp_token(), token.range);
    }
}

/// Report a doc problem without failing the parse.
///
/// A comment that does not parse is still a comment, and the tree must contain its text. Surfacing
/// the problem through the error list rather than by unwinding is what lets the editor show it
/// without the tree losing anything.
///
/// Nothing calls this yet: every rule in this layer is total, because a comment is whatever the user
/// has typed. It is kept because the alternative — deciding at each new rule that a problem is not
/// worth reporting — is how a layer ends up silently swallowing the one case that mattered.
#[allow(dead_code)]
pub(crate) fn report(p: &mut CppParser<'_>, message: &str, range: SourceRange) {
    p.push_error(CppParseError::syntax_error_from(message, range));
}
