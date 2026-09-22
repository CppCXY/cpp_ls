//! The documentation-comment lexer.
//!
//! A second lexical pass, over a comment's text rather than over the file. The C++ lexer produces
//! `// x` or `/ ** ... * /` as one token; this turns that token into the alphabet the doc grammar
//! dispatches on — command names, running text, and the punctuation Doxygen attaches meaning to.
//!
//! # Coordinates
//!
//! Every token's [`SourceRange`] is a range in the **original C++ source**, not an offset into the
//! comment. That is what lets the doc events be appended to the C++ parser's event stream directly:
//! the tree builder slices the file with them, and a consumer walking the tree finds a `@param` node
//! whose range answers "where in the file is this documented?" without any arithmetic.
//!
//! # Losslessness
//!
//! The tokens of one comment tile its text exactly — no gaps, no overlap, no dropped bytes. This is
//! not a nicety: the comment tokens are the only thing in the tree that records the comment's text
//! once the C++ lexer's token has been replaced, so a byte lost here is a byte lost from the tree.
//! `tokens_tile_the_comment` in the tests asserts it directly.
//!
//! # Prefixes, not states
//!
//! The original LDoc design carried a `LuaDocLexerState` through the parser so the lexer knew whether
//! a `-` was a comment continuation or prose. That coupling is unnecessary: a comment's *content* is
//! line-oriented, and the only thing the lexer needs to know per line is which prefix to strip. The
//! prefix is recomputed from the text at the start of each line, which keeps the lexer a pure
//! function of its input.

use crate::{
    kind::CppTokenKind,
    lexer::doc_token_kind::DocTokenKind,
    text::SourceRange,
};

/// One token of a documentation comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocToken {
    pub kind: DocTokenKind,
    pub range: SourceRange,
}

impl DocToken {
    pub fn new(kind: DocTokenKind, range: SourceRange) -> Self {
        DocToken { kind, range }
    }

    /// The `CppTokenKind` this token is recorded as in the syntax tree.
    pub fn cpp_kind(&self) -> CppTokenKind {
        self.kind.to_cpp_token()
    }
}

/// How a comment is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocCommentStyle {
    /// `//`, `///`, `//!` — ends at the end of the line.
    Line,
    /// `/*`, `/**`, `/*!` — ends at the matching `*/`.
    Block,
}

/// Is this C++ comment token written as documentation?
///
/// Doxygen's rule is about the *third* character: `///` and `//!` document, `//` does not; `/**` and
/// `/*!` document, `/*` does not. The distinction matters because a documentation block is what a
/// doc comment binds to, and treating `// ----` as documentation makes every banner a doc block.
///
/// Two exceptions, both of them things people write:
///
/// * `////` is *not* documentation — Doxygen reads the extra slashes as a separator, which is exactly
///   why that spelling is used for banners.
/// * An *empty* block comment documents nothing: `/**/` and `/*!*/` are visually identical to `/** */`
///   and people write them as blanks, so treating them as documentation would attach an empty doc
///   block to whatever follows.
pub fn is_documentation_comment(text: &str) -> bool {
    // `////` is a banner: Doxygen reads the extra slashes as a separator.
    if text.starts_with("////") {
        return false;
    }
    if text.starts_with("///") || text.starts_with("//!") {
        return true;
    }

    // A block comment documents when its third character is `*` or `!`. `/**/` and `/*!*/` are the
    // exception: the `*` there belongs to the closing `*/`, and an empty comment documents nothing.
    (text.starts_with("/**") && !text.starts_with("/**/")) || text.starts_with("/*!") && !text.starts_with("/*!*/")
}

/// Is this comment written as a block comment?
pub fn is_block_comment(text: &str) -> bool {
    text.starts_with("/*")
}

/// Lex one comment into its doc tokens.
///
/// `text` is the comment's own text and `offset` its start in the original file, so the returned
/// ranges are file coordinates.
pub fn lex_comment(text: &str, offset: usize) -> Vec<DocToken> {
    if is_block_comment(text) {
        DocLexer::new(text, offset, DocCommentStyle::Block).tokenize()
    } else {
        DocLexer::new(text, offset, DocCommentStyle::Line).tokenize()
    }
}

struct DocLexer<'a> {
    text: &'a str,
    offset: usize,
    style: DocCommentStyle,
    pos: usize,
    /// Inside a block comment, the trailing `*/` has not been written yet.
    body_end: usize,
    /// The first line of a block comment carries `/**` and must not be treated as a continuation.
    at_comment_start: bool,
    /// Is an argument list (`@param[in]`) open? While it is, text stops at `]`.
    inside_argument_list: bool,
    /// Tokens produced ahead of the cursor. Empty except while lexing a command, whose introducer
    /// and name are emitted together but returned one at a time.
    pending: Vec<DocToken>,
}

impl<'a> DocLexer<'a> {
    fn new(text: &'a str, offset: usize, style: DocCommentStyle) -> Self {
        let body_end = match style {
            DocCommentStyle::Block => {
                let opener_len = if (text.starts_with("/**") && !text.starts_with("/**/"))
                    || text.starts_with("/*!")
                {
                    3
                } else {
                    2
                };

                // The body ends where the *closing* `*/` begins, searched after the opener. Searching
                // from the opener matters: `/**/` closes at offset 2, and a search from the end that
                // assumed a body would either swallow the closer or leave it unlexed.
                text[opener_len..]
                    .find("*/")
                    .map_or(text.len(), |at| opener_len + at)
            }
            DocCommentStyle::Line => text.len(),
        };

        DocLexer {
            text,
            offset,
            style,
            pos: 0,
            body_end,
            at_comment_start: true,
            inside_argument_list: false,
            pending: Vec::new(),
        }
    }

    fn range(&self, start: usize, end: usize) -> SourceRange {
        SourceRange::new(self.offset + start, end - start)
    }

    fn token(&self, kind: DocTokenKind, start: usize, end: usize) -> DocToken {
        DocToken::new(kind, self.range(start, end))
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos.min(self.text.len())..]
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.text.get(pos..).and_then(|s| s.chars().next())
    }

    fn tokenize(mut self) -> Vec<DocToken> {
        let mut tokens = Vec::new();

        if let Some(token) = self.lex_opening() {
            tokens.push(token);
        }

        while self.pos < self.body_end || !self.pending.is_empty() {
            let produced = self.lex_one();
            if produced.is_empty() {
                break;
            }
            tokens.extend(produced);
        }

        tokens.extend(std::mem::take(&mut self.pending));

        if let Some(token) = self.lex_closing() {
            tokens.push(token);
        }

        tokens
    }

    /// The comment's introducer: `///`, `//!`, `//`, `/**`, `/*!` or `/*`.
    ///
    /// `//// x` is *not* documentation — Doxygen reads the extra slashes as a separator, which is why
    /// that spelling is what people use for banners. It opens as a plain line comment instead.
    fn lex_opening(&mut self) -> Option<DocToken> {
        let rest = self.rest();
        let (kind, len) = if rest.starts_with("///") && !rest.starts_with("////")
            || rest.starts_with("//!")
        {
            (DocTokenKind::DocLineStart, 3)
        } else if rest.starts_with("//") {
            (DocTokenKind::LineCommentStart, 2)
        } else if rest.starts_with("/**") && !rest.starts_with("/**/") || rest.starts_with("/*!") {
            (DocTokenKind::DocBlockStart, 3)
        } else if rest.starts_with("/*") {
            (DocTokenKind::BlockCommentStart, 2)
        } else {
            return None;
        };

        let start = self.pos;
        self.pos += len;
        self.at_comment_start = false;
        Some(self.token(kind, start, self.pos))
    }

    /// The `*/` of a block comment, if the C++ lexer found one.
    ///
    /// Unterminated is normal while a file is being edited, and it is not an error here: there is
    /// simply no closer to emit, and the body already covers the text that was read.
    fn lex_closing(&mut self) -> Option<DocToken> {
        if self.style != DocCommentStyle::Block {
            return None;
        }

        let start = self.body_end;
        if !self.text[start..].starts_with("*/") {
            return None;
        }

        // The body loop stops at `body_end`, so the cursor is already there; this only has to cover
        // the case where the loop never ran because the comment is nothing but `/**/`.
        self.pos = start + 2;
        Some(self.token(DocTokenKind::BlockCommentEnd, start, self.pos))
    }

    /// One token of the comment's body.
    ///
    /// Returns a `Vec` because a command's introducer and its name are two tokens produced by one
    /// lookahead. Returning them one at a time would need a "pending token" cursor in the caller;
    /// returning both keeps the lexer's output a plain left-to-right sequence, which is what the
    /// losslessness assertion and every consumer assume.
    fn lex_one(&mut self) -> Vec<DocToken> {
        if !self.pending.is_empty() {
            return std::mem::take(&mut self.pending);
        }

        let start = self.pos;

        // End of a line: the line terminator itself is one token, so that a construct that runs to
        // the end of its line has something unambiguous to stop at.
        if let Some(len) = self.line_ending_len(start) {
            self.pos = start + len;
            return vec![self.token(DocTokenKind::DocNewline, start, self.pos)];
        }

        // A block comment's continuation lines start with `*`, and a doc comment's with `* ` aligned
        // under the opening `/**`. Stripping that here is what lets the grammar see `@param x` at the
        // start of a line instead of `   * @param x` — and, more importantly, stops `@param` from
        // being read as a *reference* to a type named `*`.
        //
        // It has to run before the whitespace branch, because the prefix starts with whitespace.
        if self.at_line_start()
            && let Some(end) = self.skip_block_line_prefix(start)
            && end > start
        {
            self.pos = end;
            return vec![self.token(DocTokenKind::DocWhitespace, start, self.pos)];
        }

        // Layout.
        if self.char_at(start).is_some_and(is_doc_whitespace) {
            while self.char_at(self.pos).is_some_and(is_doc_whitespace) {
                self.pos += self.char_at(self.pos).map_or(0, char::len_utf8);
            }
            return vec![self.token(DocTokenKind::DocWhitespace, start, self.pos)];
        }

        // A command: `@` or `\` followed by a name.
        if let Some(tokens) = self.lex_command() {
            return tokens;
        }

        // Punctuation the grammar dispatches on. Anything else is text.
        if let Some((kind, len)) = self.punctuation_at(start) {
            self.pos = start + len;
            match kind {
                DocTokenKind::DocLeftBracket => self.inside_argument_list = true,
                DocTokenKind::DocRightBracket => self.inside_argument_list = false,
                DocTokenKind::DocNewline => {}
                _ => {}
            }
            return vec![self.token(kind, start, self.pos)];
        }

        vec![self.lex_text()]
    }

    fn at_line_start(&self) -> bool {
        if self.at_comment_start {
            return true;
        }
        self.pos == 0 || self.text[..self.pos].ends_with('\n') || self.text[..self.pos].ends_with('\r')
    }

    fn line_ending_len(&self, pos: usize) -> Option<usize> {
        let rest = &self.text[pos..];
        match rest.as_bytes() {
            [b'\r', b'\n', ..] => Some(2),
            [b'\r', ..] | [b'\n', ..] => Some(1),
            _ => None,
        }
    }

    /// Strip the leading `*` of a block-comment continuation line, plus the whitespace before it.
    ///
    /// Returns `None` when this line has no such prefix. The comment's own opening line is excluded:
    /// `/** @brief` has no `*` to strip, and a line that begins `**bold**` in a non-`*` comment must
    /// keep its asterisks.
    fn skip_block_line_prefix(&self, start: usize) -> Option<usize> {
        if self.style != DocCommentStyle::Block {
            return None;
        }

        let mut pos = start;
        while self.char_at(pos) == Some(' ') || self.char_at(pos) == Some('\t') {
            pos += 1;
        }

        // `*/` on its own line is the closer, not a `*` prefix followed by a slash.
        if self.text[pos..].starts_with("*/") {
            return None;
        }

        if self.char_at(pos) != Some('*') {
            return None;
        }
        pos += 1;

        // A `*` immediately followed by more punctuation is prose, not a line marker: `**bold**` and
        // `*/` are the cases that matter.
        if matches!(self.char_at(pos), Some('*') | Some('/')) {
            return None;
        }

        Some(pos)
    }

    fn lex_command(&mut self) -> Option<Vec<DocToken>> {
        let start = self.pos;
        let introducer = self.char_at(start)?;
        if introducer != '@' && introducer != '\\' {
            return None;
        }

        let name_start = start + 1;
        let mut pos = name_start;
        while self
            .char_at(pos)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            pos += 1;
        }

        if pos == name_start {
            // A bare `@` or `\` is prose — an email address, a LaTeX escape. Let the text scanner
            // take it rather than emitting a command with no name.
            return None;
        }

        self.pos = pos;

        // The introducer is its own token so the command name's range is exactly the name, which is
        // what a consumer matches against the command table.
        Some(vec![
            self.token(DocTokenKind::DocIntroducer, start, name_start),
            self.token(DocTokenKind::DocCommandName, name_start, pos),
        ])
    }

    /// Running text: everything up to the next line ending, command or block-comment end.
    ///
    /// Punctuation deliberately does *not* end a text run. `.` `:` `<` and friends only mean
    /// something to the doc grammar where a command expects them, and there the parser is looking at
    /// a token boundary the lexer already produced. Inside prose they are just characters, and
    /// breaking on them would cut `std::vector<int>` into six tokens and `user@example.com` into
    /// three.
    ///
    /// # The one exception: a run of spaces inside an argument list
    ///
    /// `@param x  the x` needs the `x` to be one token and the description another, and the only place
    /// that boundary exists is the double space. So while inside `[ ]` — which is where a command's
    /// argument starts — a run of two or more spaces ends the text run. A *single* space does not:
    /// `@param const T& x` is one argument, and splitting it would make the parameter name `const`.
    fn lex_text(&mut self) -> DocToken {
        let start = self.pos;
        let mut pos = start;

        while pos < self.body_end {
            if self.line_ending_len(pos).is_some() || self.text[pos..].starts_with("*/") {
                break;
            }
            let Some(ch) = self.char_at(pos) else {
                break;
            };
            if (ch == '@' || ch == '\\') && self.starts_a_command(pos) {
                break;
            }
            if ch == ']' && self.inside_argument_list {
                break;
            }
            if ch == ' ' && (self.spaces_start_the_body(pos) || self.spaces_end_the_comment(pos)) {
                break;
            }
            pos += ch.len_utf8();
        }

        if pos == start {
            // Nothing matched: consume one character so the caller cannot spin.
            if let Some(ch) = self.char_at(pos) {
                pos += ch.len_utf8();
            }
        }

        self.pos = pos;
        self.token(DocTokenKind::DocText, start, self.pos)
    }

    /// Does a run of space at `pos` end the comment's content — is only layout and the closer left?
    ///
    /// The trailing space of `/** @brief B */` is the comment's layout, not the brief's last character:
    /// leaving it in the text run makes the brief `B `, and a consumer rendering that prints a space
    /// that is not part of what was written. Splitting there is what keeps the two apart, and it is safe
    /// on any run length because there is nothing after it for the space to be *inside*.
    fn spaces_end_the_comment(&self, pos: usize) -> bool {
        let rest = &self.text[pos..self.body_end];
        // The body ends before the closer, so "nothing but layout left" is the whole test; a `*/` never
        // reaches here, because the loop stops on it first.
        rest.trim_start_matches([' ', '\t']).is_empty()
            || rest.trim_start_matches([' ', '\t']).starts_with("*/")
    }

    /// Does a run of space at `pos` separate an argument from the description that follows it?
    ///
    /// True for two or more spaces. Doxygen writes `@param x  the x`, and the double space is the only
    /// thing that distinguishes the name from its description once the text is one run — so without
    /// this the argument node would contain the whole phrase and a cross-reference resolving `@param x`
    /// would look up a parameter literally named `x  the x`.
    ///
    /// A *single* space does not split, or `@param const T& x` would become the parameter `const`.
    fn spaces_start_the_body(&self, pos: usize) -> bool {
        let rest = &self.text[pos..self.body_end];
        let run = rest.len() - rest.trim_start_matches(' ').len();
        run >= 2
    }

    /// Does a command start at `pos`?
    ///
    /// Two conditions, and the second is the interesting one:
    ///
    /// * a name must follow — a bare `@` or `\` is prose, so `trailing @` stays text and does not
    ///   become a command the grammar can never match;
    /// * nothing name-like may *precede* it. `user@example.com` is an address, not the `example`
    ///   command, and the difference is that the introducer is glued to a word. Requiring a
    ///   separator is a heuristic, but the alternative — checking that the word before the `@` is a
    ///   known command — would need the command table in the lexer and would still read
    ///   `see@note` wrong.
    fn starts_a_command(&self, pos: usize) -> bool {
        let ch = self.char_at(pos);
        if ch != Some('@') && ch != Some('\\') {
            return false;
        }

        let mut after = pos + 1;
        while self
            .char_at(after)
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            after += 1;
        }
        if after == pos + 1 {
            return false;
        }

        // `\` is not part of any word, so it never needs the separator check; `@` does.
        if ch == Some('@')
            && let Some(before) = self.previous_char(pos)
        {
            return !(before.is_alphanumeric() || before == '_');
        }

        true
    }

    fn previous_char(&self, pos: usize) -> Option<char> {
        self.text[..pos].chars().next_back()
    }

    fn punctuation_at(&self, pos: usize) -> Option<(DocTokenKind, usize)> {
        let rest = &self.text[pos..];
        let bytes = rest.as_bytes();

        Some(match bytes {
            [b':', b':', ..] => (DocTokenKind::DocScope, 2),
            [b'[', ..] => (DocTokenKind::DocLeftBracket, 1),
            [b']', ..] => (DocTokenKind::DocRightBracket, 1),
            [b',', ..] => (DocTokenKind::DocComma, 1),
            [b'(', ..] => (DocTokenKind::DocLeftParen, 1),
            [b')', ..] => (DocTokenKind::DocRightParen, 1),
            [b'.', ..] => (DocTokenKind::DocDot, 1),
            [b':', ..] => (DocTokenKind::DocColon, 1),
            [b'<', ..] => (DocTokenKind::DocLess, 1),
            [b'>', ..] => (DocTokenKind::DocGreater, 1),
            [b'=', ..] => (DocTokenKind::DocEquals, 1),
            _ => return None,
        })
    }
}

/// Whitespace as a comment sees it. Not `char::is_whitespace`: that includes the newline, which the
/// lexer has to keep separate because it ends a line-oriented construct.
pub fn is_doc_whitespace(ch: char) -> bool {
    ch == ' ' || ch == '\t' || ch == '\u{0b}' || ch == '\u{0c}'
}
