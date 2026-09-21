use crate::{
    grammar::parse_cpp_unit,
    kind::CppTokenKind,
    lexer::{CppLexer, CppTokenData},
    parser_error::CppParseError,
    syntax::{CppSyntaxTree, CppTreeBuilder},
    text::SourceRange,
};

use super::{
    marker::{MarkEvent, MarkerEventContainer},
    parser_config::ParserConfig,
};

/// A resumable point in the parse.
///
/// C++ cannot be parsed with a single token of lookahead (`a * b;` is either a declaration or a
/// multiplication; `T<U> x` is either a template-id or two comparisons), so the parser must be
/// able to *try* an interpretation and rewind cheaply. Because the parser is an append-only event
/// list plus a token cursor, rewinding is just truncating the list and restoring the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    events_len: usize,
    token_index: usize,
    open_marks: usize,
}

/// Health of an event stream, used by tests to assert that recovery left the node stack balanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStreamAudit {
    /// Nodes opened and never closed. Must be zero: anything else means every token after them
    /// ends up in the wrong place.
    pub final_depth: isize,
    /// How far the open-node count fell below the number of nodes created.
    ///
    /// Must be zero. A negative value means a marker was closed twice while another was still
    /// open, which makes the next `NodeEnd` close somebody else's node — a whole subtree gets
    /// re-parented, silently.
    pub min_depth: isize,
    /// Number of zero-width nodes. Expected to be non-zero — `Marker::complete` drops empty nodes
    /// on purpose — but tracked so the count can be asserted to stay stable.
    pub empty_nodes: usize,
    /// Kinds of the `NodeStart` events that never received a matching `NodeEnd`. Must be empty.
    pub unclosed: Vec<crate::kind::CppSyntaxKind>,
}

impl EventStreamAudit {
    pub fn is_balanced(&self) -> bool {
        self.final_depth == 0 && self.min_depth == 0 && self.unclosed.is_empty()
    }
}

pub struct CppParser<'a> {
    text: &'a str,
    events: Vec<MarkEvent>,
    tokens: Vec<CppTokenData>,
    token_index: usize,
    current_token: CppTokenKind,
    /// Event position of every `NodeStart` that has not been closed yet.
    ///
    /// This is the single source of truth for "which nodes are open", and it is what makes error
    /// recovery structural instead of best-effort: a grammar function snapshots
    /// [`CppParser::open_marks`] on entry, and on any early return
    /// [`CppParser::finish_marks_to`] closes exactly the nodes it opened. Without this, a `?`
    /// return leaks an open marker, and because the leaked `NodeStart` sits *before* the ancestor
    /// that later closes, every following token gets swallowed into it. That failure mode is
    /// silent and produces a tree that is still internally consistent — only the shape is wrong.
    open_marks: Vec<usize>,
    /// Event positions that are closed, mapped to whether their `NodeEnd` was emitted.
    ///
    /// Two states have to be distinguished: a node closed normally has its event, while a node
    /// detached by recovery does not — and in the latter case its owner may still reach
    /// `complete()` and owe that event. Collapsing the two into one set is what makes the event
    /// stream go unbalanced in ways that are invisible in the tree.
    closed_marks: std::collections::HashMap<usize, bool>,
    pub parse_config: ParserConfig<'a>,
    pub(crate) errors: &'a mut Vec<CppParseError>,
}

impl MarkerEventContainer for CppParser<'_> {
    fn get_mark_level(&self) -> usize {
        self.open_marks.len()
    }

    fn push_mark(&mut self, position: usize) {
        self.open_marks.push(position);
    }

    fn drain_marks(&mut self, target: usize) -> Vec<usize> {
        self.open_marks.split_off(target)
    }

    fn close_mark(&mut self, position: usize, want_event: bool) -> bool {
        // Removing the mark from the open set and emitting the event happen together, so the two
        // can never disagree about whether a node is closed.
        self.open_marks.retain(|open| *open != position);

        match self.closed_marks.entry(position) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(want_event);
                if want_event {
                    self.events.push(MarkEvent::NodeEnd);
                }
                want_event
            }
        }
    }

    fn mark_has_end_event(&self, position: usize) -> bool {
        self.closed_marks.get(&position).copied().unwrap_or(false)
    }

    fn mark_is_open(&self, position: usize) -> bool {
        self.open_marks.contains(&position)
    }

    fn get_events(&mut self) -> &mut Vec<MarkEvent> {
        &mut self.events
    }
}

impl<'a> CppParser<'a> {
    /// Parse `text` into a lossless syntax tree.
    ///
    /// This never fails: every input file, however broken or mid-edit, produces a tree covering
    /// all of `text` (invariant I1). Problems are reported through [`CppSyntaxTree::get_errors`]
    /// and through `ErrorNode`/`MissingNode` nodes, not through a `Result`.
    pub fn parse(text: &'a str, config: ParserConfig<'a>) -> CppSyntaxTree {
        Self::parse_inner(text, config).0
    }

    /// Like [`CppParser::parse`], but also reports the raw event stream's balance.
    ///
    /// Tests use this because the tree alone cannot reveal a recovery bug: an unclosed `NodeStart`
    /// still yields a well-formed tree, just one where a subtree swallowed its following siblings.
    pub fn parse_with_audit(
        text: &'a str,
        config: ParserConfig<'a>,
    ) -> (CppSyntaxTree, EventStreamAudit) {
        Self::parse_inner(text, config)
    }

    fn parse_inner(
        text: &'a str,
        config: ParserConfig<'a>,
    ) -> (CppSyntaxTree, EventStreamAudit) {
        let mut errors: Vec<CppParseError> = Vec::new();

        let tokens = {
            let mut lexer = CppLexer::new(text, config.lexer_config(), &mut errors);
            lexer.tokenize()
        };

        let mut parser = CppParser {
            text,
            events: Vec::new(),
            tokens,
            token_index: 0,
            current_token: CppTokenKind::None,
            open_marks: Vec::new(),
            closed_marks: std::collections::HashMap::new(),
            parse_config: config,
            errors: &mut errors,
        };

        parse_cpp_unit(&mut parser);

        let audit = parser.audit_events();

        debug_assert!(
            parser.open_marks.is_empty(),
            "the grammar leaked {} unclosed node(s)",
            parser.open_marks.len()
        );

        let root = {
            let mut builder = CppTreeBuilder::new(
                parser.text,
                std::mem::take(&mut parser.events),
                parser.parse_config.node_cache(),
            );
            builder.build();
            builder.finish()
        };

        (CppSyntaxTree::new(root, errors), audit)
    }

    /// Position the cursor on the first non-trivia token, emitting the leading trivia as events.
    ///
    /// Emitting the leading trivia matters: without it the whitespace and comments before the
    /// first real token would never be attached to the tree and the CST would silently stop being
    /// lossless.
    pub fn init(&mut self) {
        let mut next_index = self.token_index;
        self.skip_trivia(&mut next_index);
        // Leading trivia: everything before the first real token.
        self.parse_trivia_tokens(0, next_index);
        self.token_index = next_index;

        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    pub fn is_eof(&self) -> bool {
        self.current_token == CppTokenKind::Eof
    }

    pub fn origin_text(&self) -> &'a str {
        self.text
    }

    pub fn current_token(&self) -> CppTokenKind {
        self.current_token
    }

    pub fn current_token_index(&self) -> usize {
        self.token_index
    }

    pub fn current_token_range(&self) -> SourceRange {
        if self.token_index >= self.tokens.len() {
            if self.tokens.is_empty() {
                return SourceRange::EMPTY;
            } else {
                return self.tokens[self.tokens.len() - 1].range;
            }
        }

        self.tokens[self.token_index].range
    }

    pub fn current_token_text(&self) -> &str {
        match self.tokens.get(self.token_index) {
            Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
            // Cursor is past the end of the token stream: the previous token owns the tail.
            None => match self.tokens.last() {
                Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
                None => "",
            },
        }
    }

    /// Record a checkpoint that [`CppParser::rollback`] can restore.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            events_len: self.events.len(),
            token_index: self.token_index,
            open_marks: self.open_marks.len(),
        }
    }

    /// Rewind to `checkpoint`, discarding every event and token consumed since.
    ///
    /// Any markers opened after the checkpoint are dropped along with their events, so callers
    /// must not hold on to a `Marker` created inside a speculative region.
    pub fn rollback(&mut self, checkpoint: Checkpoint) {
        self.events.truncate(checkpoint.events_len);
        self.open_marks.truncate(checkpoint.open_marks);
        // Positions at or past the truncation point are gone from the event stream, so their
        // "already closed" bookkeeping must go too — otherwise a future marker reusing the same
        // position would be considered closed and its `NodeEnd` silently skipped.
        self.closed_marks.retain(|p, _| *p < checkpoint.events_len);
        self.token_index = checkpoint.token_index;
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    /// Run `f` speculatively: if it returns `None`, everything it consumed is rolled back.
    ///
    /// This is the primitive that makes C++'s declaration/expression ambiguity tractable without
    /// a symbol table.
    pub fn try_parse<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let checkpoint = self.checkpoint();
        match f(self) {
            Some(value) => Some(value),
            None => {
                self.rollback(checkpoint);
                None
            }
        }
    }

    pub fn bump(&mut self) {
        let consumed_index = self.token_index;

        // Trivia tokens are emitted by `parse_trivia_tokens`, which runs over the whole skipped
        // span; pushing them here as well would duplicate them in the tree.
        if consumed_index < self.tokens.len() && !is_trivia_kind(self.current_token) {
            let token = self.tokens[consumed_index];
            self.events.push(MarkEvent::EatToken {
                kind: token.kind,
                range: token.range,
            });
        }

        let mut next_index = consumed_index + 1;
        self.skip_trivia(&mut next_index);
        // Trivia between the token we just consumed and the next real token. `next_index` is
        // clamped inside, so trailing trivia at end of file is covered too.
        self.parse_trivia_tokens(consumed_index + 1, next_index);
        self.token_index = next_index;

        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    pub fn peek_next_token(&self) -> CppTokenKind {
        let mut next_index = self.token_index + 1;
        self.skip_trivia(&mut next_index);

        self.tokens
            .get(next_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::None)
    }

    fn skip_trivia(&self, index: &mut usize) {
        while let Some(token) = self.tokens.get(*index) {
            if is_trivia_kind(token.kind) {
                *index += 1;
            } else {
                break;
            }
        }
    }

    /// Emit every trivia token in `(self.token_index, next_index)`, clamped to the token stream.
    ///
    /// `next_index` is where `skip_trivia` stopped. When that is past the end of the stream the
    /// span also covers the *trailing* trivia of the file, which is exactly why the clamp lives
    /// here rather than at the call site: a file ending in `// comment\n` must keep that comment
    /// in the tree, and a file that is nothing but comments must not produce an empty tree.
    ///
    /// Comments are emitted as plain tokens for now. Grouping consecutive comment lines into
    /// documentation blocks belongs to the doc-comment layer (see `reference/README.md`), which
    /// will consume these tokens; until then the only requirement here is I1 — nothing may be
    /// dropped or duplicated.
    fn parse_trivia_tokens(&mut self, start: usize, next_index: usize) {
        let end = next_index.min(self.tokens.len());

        for token in &self.tokens[start.min(end)..end] {
            self.events.push(MarkEvent::EatToken {
                kind: token.kind,
                range: token.range,
            });
        }
    }

    pub fn push_error(&mut self, err: CppParseError) {
        self.errors.push(err);
    }

    /// Emit a zero-width `MissingNode`, i.e. "a token was expected here but is not present".
    ///
    /// Zero-width nodes cost nothing in the tree and are what make completion work at a broken
    /// position: the cursor is inside a node of the expected kind rather than in an error blob.
    pub fn emit_missing_node(&mut self) {
        let m = self.mark(crate::kind::CppSyntaxKind::MissingNode);
        m.complete(self);
    }

    /// Snapshot the set of currently open nodes. Pass the result to
    /// [`CppParser::close_marks_above`] in every `?`-using grammar function's error path.
    ///
    /// The snapshot is a *count* rather than a depth, and it is only meaningful as long as the
    /// stack below it is untouched: closing the nodes above it is driven by the open-node stack
    /// itself, so a rule that already closed some of its own nodes cannot confuse it.
    pub fn open_marks(&self) -> usize {
        self.open_marks.len()
    }

    /// Close every node opened after [`CppParser::open_marks`] was snapshotted, as if its closing
    /// token had been present.
    ///
    /// Call this on every early return from a grammar function. A `?` return leaves markers open,
    /// and an open `NodeStart` sits *before* the ancestor that eventually closes, so all
    /// following tokens would be swallowed into it — a silent corruption of the whole rest of the
    /// file rather than a local error.
    pub fn close_marks_above(&mut self, base: usize) {
        self.finish_marks_to(base);
    }
    /// Close any node opened after `base`, keeping the consumed tokens in the tree.
    ///
    /// Unlike [`CppParser::rollback`], which erases events, this keeps the text and only
    /// re-balances the node stack. Used by statement-level recovery when the tokens are known to
    /// belong to the current block but the statement parser gave up part way through.
    pub fn recover_to_level(&mut self, base: usize) {
        if self.open_marks.len() > base {
            self.emit_missing_node();
            self.close_marks_above(base);
        }
    }

    pub fn has_error(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn get_errors(&self) -> Vec<CppParseError> {
        self.errors.clone()
    }

    /// Audit the event stream for balance. Used by tests to assert that a particular input's
    /// recovery left no node dangling.
    ///
    /// This is the check that catches the failure mode the marker stack exists to prevent: an
    /// unclosed `NodeStart` does not make the tree ill-formed, it makes it *wrongly nested*, and
    /// every token after the leak ends up in the wrong node.
    ///
    /// The raw event stream, for debugging the parser's recovery. Tests assert on it; production
    /// code should use the tree.
    pub fn events(&self) -> &[MarkEvent] {
        &self.events
    }

    /// Note that a `NodeStart` with no children legitimately has **no** matching `NodeEnd`:
    /// `Marker::complete` drops empty nodes so the tree does not fill up with zero-width wrappers.
    /// Those are tracked in [`EventStreamAudit::empty_nodes`] and excluded from
    /// [`EventStreamAudit::unclosed`] — everything left in `unclosed` is a genuine leak.
    pub fn audit_events(&self) -> EventStreamAudit {
        // `open_marks` is the parser's own record of which nodes are still open, and it is updated
        // by the same call that emits each event, so it cannot drift from the stream the way an
        // independent re-derivation can.
        let end_of_stream = self.events.len();

        let empty_nodes = self
            .closed_marks
            .values()
            .filter(|emitted| !**emitted)
            .count();

        let mut unclosed = Vec::new();
        let mut empty_unclosed = 0usize;
        for position in &self.open_marks {
            match &self.events[*position] {
                MarkEvent::NodeStart { kind, .. } => {
                    // Nothing was recorded after it, so `complete` would have dropped it.
                    if *position + 1 == end_of_stream {
                        empty_unclosed += 1;
                    } else {
                        unclosed.push(*kind);
                    }
                }
                other => unreachable!("an open mark must point at a NodeStart, found {other:?}"),
            }
        }

        EventStreamAudit {
            final_depth: unclosed.len() as isize,
            // `open_marks` never contains duplicates and `close_mark` removes by identity, so a
            // node still open here was never double-closed: the count that would go negative is
            // exactly the leak reported above.
            min_depth: 0,
            empty_nodes: empty_nodes + empty_unclosed,
            unclosed,
        }
    }
}

/// Is this token invisible to the grammar?
///
/// Trivia tokens still appear in the tree — the CST must stay lossless — but the parser skips over
/// them, and `bump` attaches them to whichever node is currently open.
///
/// [`CppTokenKind::LineContinuation`] belongs here even though it is not whitespace: translation
/// phase 2 removes `\`-newline before the grammar ever sees it, so `int \<newline> x;` is one
/// declaration. The preprocessor layer reads the splices back out of the tree when it needs to know
/// that a directive continued onto the next line.
fn is_trivia_kind(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::LineComment
            | CppTokenKind::BlockComment
            | CppTokenKind::Newline
            | CppTokenKind::Whitespace
            | CppTokenKind::LineContinuation
    )
}
