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
    /// How many template argument lists the cursor is inside, syntactically.
    ///
    /// Inside one, `>` closes the list rather than comparing: `Vec<A<B>>`, `Vec<1, 2>`. The
    /// expression grammar cannot know that, so it asks. A counter rather than a flag because the
    /// lists nest, and the innermost closer belongs to the innermost list.
    template_argument_depth: usize,
    /// Did the declarator parsed most recently declare a function?
    ///
    /// Set by [`crate::grammar::cpp::types::parse_declarator`] and read by
    /// [`crate::grammar::cpp::decls::finish_init_declarator`], which has to decide whether the `{`
    /// at the cursor opens a function body or a brace initializer.
    ///
    /// This is recorded rather than inferred from the event stream because the distinguishing
    /// event can be *dropped*: `void f()` has an empty `ParameterList`, which `Marker::complete`
    /// discards as a zero-width node, leaving the events of `void f() {}` indistinguishable from
    /// those of `int x {}`. Reading it off the parse directly is the only answer that survives that.
    last_declarator_is_function: bool,
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

    fn mark_was_detached(&self, position: usize) -> bool {
        matches!(self.closed_marks.get(&position), Some(false))
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

    /// Like [`CppParser::parse`], but also returns the raw event stream.
    ///
    /// The event stream is the parser's real output and the tree is a fold of it, so when a tree is
    /// wrongly nested the stream is where the cause is visible. Kept public because diagnosing a
    /// shape bug otherwise means adding a temporary `println!` inside the parser.
    pub fn parse_with_events(
        text: &'a str,
        config: ParserConfig<'a>,
    ) -> (CppSyntaxTree, Vec<MarkEvent>) {
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
            template_argument_depth: 0,
            last_declarator_is_function: false,
            parse_config: config,
            errors: &mut errors,
        };

        parse_cpp_unit(&mut parser);

        let events = std::mem::take(&mut parser.events);
        let root = {
            let mut builder = crate::syntax::CppTreeBuilder::new(text, events.clone(), None);
            builder.build();
            builder.finish()
        };

        let tree = CppSyntaxTree::new(root, errors);
        (tree, events)
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
            template_argument_depth: 0,
            last_declarator_is_function: false,
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

    /// Split the current token into two, consuming the first part and leaving the remainder at the
    /// cursor.
    ///
    /// C++ needs this exactly once, but it needs it badly: `std::vector<std::vector<int>>` ends in a
    /// single `>>` token that has to close two template argument lists. The alternative — lexing `>`
    /// one character at a time and letting the parser join them — is worse, because then `a >> b`
    /// costs a merge on every shift. Splitting on demand keeps the common case free and puts the
    /// decision where the context actually is.
    ///
    /// `consumed` is the length of the first part in bytes and `first_kind` its kind — the first half
    /// of `>>` is a `>`, not a `>>`, and keeping the original kind would leave a one-byte token still
    /// claiming to be a shift operator. Every later token-level decision (counting angle-bracket
    /// depth, deciding whether a `>` closes a list) would then be wrong. The second token is inserted
    /// after the cursor, so the caller's current token is the first part and its index is unchanged.
    pub fn split_current_token(
        &mut self,
        consumed: usize,
        first_kind: CppTokenKind,
        remainder_kind: CppTokenKind,
    ) {
        let Some(token) = self.tokens.get(self.token_index).copied() else {
            return;
        };

        let first_end = token.range.start_offset + consumed;
        if consumed == 0 || first_end >= token.range.end_offset() {
            // Nothing to split: either the whole token is consumed or the request is degenerate.
            return;
        }

        let first = CppTokenData::new(
            first_kind,
            crate::text::SourceRange::new(token.range.start_offset, consumed),
        );
        let second = CppTokenData::new(
            remainder_kind,
            crate::text::SourceRange::new(first_end, token.range.end_offset() - first_end),
        );

        self.tokens[self.token_index] = first;
        self.tokens.insert(self.token_index + 1, second);
        self.current_token = first.kind;
    }

    /// Consume the current token, treating only its first `consumed` bytes as consumed.
    pub fn bump_prefix_of_current_token(&mut self, consumed: usize, first_kind: CppTokenKind) {
        self.split_current_token(consumed, first_kind, CppTokenKind::Greater);
        self.bump();
    }

    /// Try to consume a header name at the cursor, folding the `<`, name and `>` tokens the ordinary
    /// sweep produced into a single [`CppTokenKind::HeaderName`].
    ///
    /// Only the parser knows when a header name is expected (`#include` and friends), and only the
    /// parser can rewrite the token stream, so the two halves of header-name lexing live on either
    /// side of this call. Returns whether a header name was consumed; on `false` the caller should
    /// lex the token normally.
    pub fn try_lex_header_name(&mut self) -> bool {
        let Some(start) = self.tokens.get(self.token_index) else {
            return false;
        };

        // A quoted header name survives the ordinary sweep as a string literal, because the lexer
        // cannot tell it apart from a string at the time. `"local.h"` is already the right shape, so
        // this case only has to relabel it.
        if start.kind == CppTokenKind::StringLiteral {
            let text = &self.text[start.range.start_offset..start.range.end_offset()];
            // A header name has no escapes and no concatenation; anything else is a real string.
            if !text.contains('\\') {
                let token = self.tokens[self.token_index];
                self.tokens[self.token_index] = CppTokenData::new(CppTokenKind::HeaderName, token.range);
                self.current_token = CppTokenKind::HeaderName;
                self.bump();
                return true;
            }
            return false;
        }

        if start.kind != CppTokenKind::Less {
            return false;
        }

        // Angle form: scan forward for the closing `>` at the same "line", refusing if anything
        // shows up that cannot be inside a header name.
        let mut end_index = self.token_index + 1;
        let mut consumed = 0usize;
        loop {
            let Some(token) = self.tokens.get(end_index) else {
                return false;
            };

            match token.kind {
                CppTokenKind::Greater => break,
                CppTokenKind::Whitespace => {}
                // Anything else invalidates the guess and the caller lexes normally.
                CppTokenKind::Identifier
                | CppTokenKind::Dot
                | CppTokenKind::Slash
                | CppTokenKind::Minus
                | CppTokenKind::Plus
                | CppTokenKind::IntegerLiteral => {}
                _ => return false,
            }

            consumed += 1;
            end_index += 1;
        }

        if consumed == 0 {
            return false;
        }

        let close = self.tokens[end_index];
        let whole = CppTokenData::new(
            CppTokenKind::HeaderName,
            crate::text::SourceRange::new(
                start.range.start_offset,
                close.range.end_offset() - start.range.start_offset,
            ),
        );

        // Replace the whole run with the single header-name token and advance past it.
        self.tokens.splice(self.token_index..=end_index, [whole]);
        self.current_token = CppTokenKind::HeaderName;
        self.bump();
        true
    }

    /// Does any node opened at or after `from_event` have one of these kinds?
    ///
    /// Used by the grammar to answer "what did I just parse?" without threading a return value
    /// through every level. `from_event` is an event index from
    /// [`CppParser::current_event_count`].
    pub fn events_contain_any(
        &self,
        from_event: usize,
        kinds: &[crate::kind::CppSyntaxKind],
    ) -> bool {
        self.events[from_event.min(self.events.len())..]
            .iter()
            .any(|event| {
                matches!(
                    event,
                    MarkEvent::NodeStart { kind, .. } if kinds.contains(kind)
                )
            })
    }

    /// Number of events recorded so far, for use as a `from_event` bound.
    pub fn current_event_count(&self) -> usize {
        self.events.len()
    }

    /// Is the cursor inside a template argument list, as far as the grammar has descended?
    ///
    /// Inside one, `>` closes the list instead of comparing, so the expression grammar must not
    /// treat it as an operator. `parse_template_argument_list` is the only place that sets this, via
    /// [`crate::grammar::cpp::types::TemplateArgumentScope`].
    pub fn is_in_template_arguments(&self) -> bool {
        self.template_argument_depth > 0
    }

    /// Enter a template argument list. Returns the previous depth so the caller can restore it.
    pub fn enter_template_arguments(&mut self) -> usize {
        let previous = self.template_argument_depth;
        self.template_argument_depth += 1;
        previous
    }

    /// Leave a template argument list, restoring the depth `enter_template_arguments` returned.
    pub fn leave_template_arguments(&mut self, previous: usize) {
        self.template_argument_depth = previous;
    }

    /// Record whether the declarator just parsed declared a function. See the field's docs.
    pub fn set_last_declarator_is_function(&mut self, is_function: bool) {
        self.last_declarator_is_function = is_function;
    }

    /// Did the declarator parsed most recently declare a function?
    pub fn last_declarator_is_function(&self) -> bool {
        self.last_declarator_is_function
    }

    /// Text of the significant token at relative offset `offset` from the cursor. Empty past the end.
    ///
    /// The companion to [`CppParser::peek_token_kind_at`], needed because `module` and `import` are
    /// *contextual* keywords: they must be recognised by spelling, not by kind.
    pub fn peek_token_text_at(&self, offset: usize) -> &str {
        let mut index = self.token_index;

        for current in 0..=offset {
            self.skip_trivia(&mut index);
            if current == offset {
                return match self.tokens.get(index) {
                    Some(token) => {
                        &self.text[token.range.start_offset..token.range.end_offset()]
                    }
                    None => "",
                };
            }
            index += 1;
        }

        ""
    }

    /// Kind of the token at `index`, ignoring trivia. `None` past the end.
    pub fn token_kind_at(&self, index: usize) -> CppTokenKind {
        self.tokens
            .get(index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::None)
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

    /// Kinds of the next `range.len()` significant tokens, starting at the cursor.
    ///
    /// Trivia is skipped, so this is "the next few things the grammar will see". Shorter than
    /// `range` near end of input, and padded with [`CppTokenKind::None`] so callers can index it
    /// without a length check.
    ///
    /// # Comparing the result
    ///
    /// This returns a `Vec`, and `Vec<T> == [T; N]` is **always false** in Rust — there is no
    /// `PartialEq<[T; N]>` impl. A comparison written as `peek_token_kind_at(1..2) == [X]` therefore
    /// compiles and silently never matches, which is exactly the kind of bug that reads as "the
    /// branch is dead" rather than "the comparison is wrong". Call `.as_slice()` or use the
    /// single-token helpers ([`CppParser::peek_next_token`], [`CppParser::peek_token_text_at`]).
    pub fn peek_token_kind_at(&self, range: std::ops::Range<usize>) -> Vec<CppTokenKind> {
        let mut kinds = Vec::with_capacity(range.len());
        let mut index = self.token_index;

        for offset in 0..range.end {
            self.skip_trivia(&mut index);
            if offset < range.start {
                index += 1;
                continue;
            }

            match self.tokens.get(index) {
                Some(token) => kinds.push(token.kind),
                None => break,
            }
            index += 1;
        }

        kinds.resize(range.len(), CppTokenKind::None);
        kinds
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
    /// # Comments are not emitted as tokens
    ///
    /// Whitespace and newlines go into the stream as themselves. A comment does not: it is handed to
    /// the documentation layer ([`crate::grammar::doc::parse_comment_group`]), which re-lexes its text
    /// with the doc lexer and emits a `DocComment` node — and, in it, the Doxygen commands the comment
    /// contains. Those events go into this same stream, so a doc node is an ordinary child of whatever
    /// node was open when the comment was found.
    ///
    /// The invariant this must not break is I1: every byte of the file appears exactly once in the
    /// tree. It holds because the doc lexer tiles a comment's text exactly (asserted in
    /// `tests/doc_lexer.rs`), so replacing the comment's one token with its doc tokens is a
    /// lossless rewrite rather than a substitution.
    ///
    /// # One node per run of adjacent comments
    ///
    /// A document is written across several `///` lines, so the comments that belong together are
    /// parsed as one group and produce one `DocComment` node. "Adjacent" means nothing but whitespace
    /// between them and at most one line apart: a blank line ends the run, which is how Doxygen
    /// separates a declaration's documentation from the block above it.
    fn parse_trivia_tokens(&mut self, start: usize, next_index: usize) {
        let end = next_index.min(self.tokens.len());
        let mut index = start.min(end);

        while index < end {
            let token = self.tokens[index];

            if !is_comment_kind(token.kind) {
                self.events.push(MarkEvent::EatToken {
                    kind: token.kind,
                    range: token.range,
                });
                index += 1;
                continue;
            }

            let group = self.comment_group_at(index, end);
            let sources = self.comment_sources(&group);
            // The doc parse is total: it cannot fail, and it cannot leave a marker open, so there is
            // nothing to recover from and nothing to check. Anything it wants to report it reports
            // through `push_error`.
            let _ = crate::grammar::doc::parse_comment_group(self, &sources);

            index = *group.last().expect("a group has at least one comment") + 1;
        }
    }

    /// Build the doc layer's view of one comment group.
    ///
    /// The separators are the trivia *between* the comments, collected here because this is where the
    /// token list is. They are handed to the doc parser rather than emitted here so that they land
    /// inside the `DocComment` node — see [`crate::grammar::doc::CommentSource`].
    fn comment_sources(&self, group: &[usize]) -> Vec<crate::grammar::doc::CommentSource> {
        let mut sources = Vec::with_capacity(group.len());

        for (position, &token_index) in group.iter().enumerate() {
            let comment = self.tokens[token_index];
            let mut source = crate::grammar::doc::CommentSource::first(comment.range);

            if position > 0 {
                // Everything between the previous comment and this one. The group is built from
                // comment positions plus the layout between them, so this is that layout — and only
                // layout, since a non-layout token would have ended the group.
                let previous_end = self.tokens[group[position - 1]].range.end_offset();
                for token in self.tokens[..token_index].iter().filter(|token| {
                    is_line_layout_kind(token.kind) && token.range.start_offset >= previous_end
                }) {
                    if source.separator_len < crate::grammar::doc::MAX_SEPARATOR_TOKENS {
                        source.separator[source.separator_len] = Some(token.range);
                        source.separator_len += 1;
                    }
                }
            }

            sources.push(source);
        }

        sources
    }

    /// The comments forming one documentation group, starting at `start`.
    ///
    /// `start` must be a comment. The returned positions are ascending and non-empty, and the first is
    /// `start`.
    fn comment_group_at(&self, start: usize, end: usize) -> Vec<usize> {
        let mut group = vec![start];
        let mut index = start + 1;
        let mut previous = self.tokens[start];

        while index < end {
            let token = self.tokens[index];

            if is_comment_kind(token.kind) {
                if self.lines_between(previous.range.end_offset(), token.range.start_offset) <= 1 {
                    group.push(index);
                    previous = token;
                    index += 1;
                    continue;
                }
                break;
            }

            // Only whitespace may separate the comments of a group, and never more than one line.
            if !is_line_layout_kind(token.kind)
                || self.lines_between(previous.range.end_offset(), token.range.start_offset) > 1
            {
                break;
            }

            index += 1;
        }

        group
    }

    /// How many line breaks separate two offsets?
    ///
    /// `0` means "on the same line", which is how `/* a */ /* b */` stays one group. Counting the
    /// newlines between them rather than asking a line index is both simpler and more accurate here:
    /// the question is about the gap, not about where the lines are.
    fn lines_between(&self, from: usize, to: usize) -> usize {
        if to <= from {
            return 0;
        }

        self.text[from..to.min(self.text.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    }

    pub fn push_error(&mut self, err: CppParseError) {
        self.errors.push(err);
    }

    /// Append a token produced by the documentation layer to the event stream.
    ///
    /// The doc layer hands over a token that already carries a range in *file* coordinates, so this
    /// is the whole of the translation between the two layers: no offset arithmetic, no buffering.
    /// That is what lets doc nodes be ordinary children of the C++ tree.
    pub(crate) fn push_doc_token(&mut self, kind: CppTokenKind, range: crate::text::SourceRange) {
        self.events.push(MarkEvent::EatToken { kind, range });
    }

    /// Emit the current token and move on, **without** attaching the trivia that follows it.
    ///
    /// `bump` is the right call almost everywhere: trivia belongs to the construct it sits inside.
    /// A grammar rule that wants to close its node before that trivia is emitted — because the trivia
    /// belongs to the *enclosing* construct — advances with this and then calls
    /// [`CppParser::emit_trivia_after_current_token`].
    pub fn consume_current_token(&mut self) {
        let consumed_index = self.token_index;

        if consumed_index < self.tokens.len() && !is_trivia_kind(self.current_token) {
            let token = self.tokens[consumed_index];
            self.events.push(MarkEvent::EatToken {
                kind: token.kind,
                range: token.range,
            });
        }

        self.token_index = consumed_index + 1;
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    /// Consume the current token with [`CppParser::consume_current_token`] if it has this kind.
    ///
    /// Returns whether it was consumed. Used for tokens a rule requires but wants to treat as
    /// optional at the point of consumption, so that a missing one is reported by the caller rather
    /// than aborting a construct that is otherwise complete.
    pub fn consume_current_token_if(&mut self, kind: CppTokenKind) -> bool {
        if self.current_token == kind {
            self.consume_current_token();
            true
        } else {
            false
        }
    }

    /// Emit the trivia the cursor is sitting on, without consuming any significant token.
    ///
    /// The counterpart to [`CppParser::consume_current_token`]: after a rule has closed its node, the
    /// layout it left behind still has to reach the tree — the CST is lossless — and this is what puts
    /// it into whichever node is open now.
    pub fn emit_trivia_after_current_token(&mut self) {
        let trivia_start = self.token_index;
        let mut next_index = trivia_start;
        self.skip_trivia(&mut next_index);
        self.parse_trivia_tokens(trivia_start, next_index);

        self.token_index = next_index;
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
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
    is_comment_kind(kind) || is_line_layout_kind(kind)
}

/// Is this a comment? These are the tokens the documentation layer takes over.
fn is_comment_kind(kind: CppTokenKind) -> bool {
    matches!(kind, CppTokenKind::LineComment | CppTokenKind::BlockComment)
}

/// Is this layout *within* a line, or the break that ends one?
///
/// Layout tokens are what may sit between two comments of the same documentation group. A line
/// continuation is included because a `\`-spliced comment is still one comment as far as the
/// preprocessor is concerned.
fn is_line_layout_kind(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace | CppTokenKind::Newline | CppTokenKind::LineContinuation
    )
}
