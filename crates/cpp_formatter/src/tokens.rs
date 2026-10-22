//! **The token stream a formatter needs**: every token, and the trivia between them.
//!
//! The parser's job is to build a tree, so it *skips* trivia; a formatter's job is to rewrite trivia, so it needs
//! the trivia to be data. `cpp_parser::lex` gives the full stream (`Whitespace`, `Newline`, `LineContinuation`,
//! `LineComment`, `BlockComment` are all there), and this module splits it into
//!
//! ```text
//! tokens[i]   the i-th significant token (its text is a slice of the source)
//! gaps[i]     the trivia between tokens[i-1] and tokens[i]: the newlines, and **the comments in order**
//! trailing    the trivia after the last token (usually the file's final newline)
//! ```
//!
//! # Why the comments are separated from the whitespace
//!
//! Because a comment is *content*: the formatter may move it to a new line or indent it, but it must not change one
//! byte of its text. Keeping comments as ordered items inside the gap lets the layout put each one on its own line
//! (or after the previous token, when it was written that way) without ever looking inside the comment's text.

use cpp_parser::{CppTokenKind, LexerConfig};

/// One significant token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: CppTokenKind,
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

/// One comment, and where it stood relative to the tokens around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comment<'a> {
    pub kind: CppTokenKind,
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
    /// A newline stood between whatever came before it and this comment: it was written on its own line.
    pub own_line: bool,
    /// A newline stood between this comment and whatever comes after it.
    pub ends_line: bool,
}

impl Comment<'_> {
    /// A `//` comment: anything written after it on the same line is part of the comment, so a newline is
    /// mandatory after it.
    pub fn is_line_comment(&self) -> bool {
        self.kind == CppTokenKind::LineComment
    }
}

/// The trivia between two tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gap<'a> {
    pub start: usize,
    pub end: usize,
    /// How many newlines the *whitespace* of this gap contains (comments' own newlines are not counted here).
    pub newlines: usize,
    pub comments: Vec<Comment<'a>>,
    /// The gap ended the file (nothing after it).
    pub at_end: bool,
}

impl Gap<'_> {
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A file split for formatting.
pub struct Stream<'a> {
    pub source: &'a str,
    pub tokens: Vec<Token<'a>>,
    /// `gaps[i]` is before `tokens[i]`; `gaps.len() == tokens.len() + 1`, the last one being `trailing`.
    pub gaps: Vec<Gap<'a>>,
}

/// What line ending the file uses. A formatter that turned CRLF into LF would rewrite every line of a file it was
/// asked to "only re-indent".
pub fn line_ending(source: &str) -> &'static str {
    if source.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Split `source` into tokens and gaps.
pub fn stream(source: &str) -> Stream<'_> {
    let (lexed, _errors) = cpp_parser::lex(source, &LexerConfig::default());

    let mut tokens: Vec<Token<'_>> = Vec::new();
    let mut gaps: Vec<Gap<'_>> = Vec::new();
    // The trivia of the gap being built: (start, end) of the whole gap, and the items in order.
    let mut gap_start = 0usize;
    let mut gap_end = 0usize;
    let mut newlines = 0usize;
    let mut comments: Vec<Comment<'_>> = Vec::new();

    // Trivia before the first token belongs to `gaps[0]`.
    for token in &lexed {
        let start = token.range.start_offset;
        let end = token.range.start_offset + token.range.length;
        let text = source.get(start..end).unwrap_or("");

        match token.kind {
            CppTokenKind::Whitespace => gap_end = end,
            CppTokenKind::Newline => {
                gap_end = end;
                newlines += 1;
            }
            CppTokenKind::LineContinuation => {
                // A splice is not a line break for the *layout*: the next line belongs to the same logical line.
                gap_end = end;
            }
            CppTokenKind::LineComment | CppTokenKind::BlockComment => {
                // A newline between the previous comment and this one: the previous comment ended its line.
                if let Some(previous) = comments.last_mut() {
                    previous.ends_line = newlines > 0;
                }
                let own_line = newlines > 0 || comments.is_empty() && gap_end == gap_start;
                comments.push(Comment {
                    kind: token.kind,
                    text,
                    start,
                    end,
                    own_line,
                    ends_line: false,
                });
                gap_end = end;
                newlines = 0;
            }
            _ => {
                // A significant token: close the gap and push the token.
                if let Some(last) = comments.last_mut() {
                    // A newline between the last comment and this token.
                    last.ends_line = newlines > 0;
                }
                gaps.push(Gap {
                    start: gap_start,
                    end: gap_end,
                    newlines,
                    comments: std::mem::take(&mut comments),
                    at_end: false,
                });
                tokens.push(Token {
                    kind: token.kind,
                    text,
                    start,
                    end,
                });
                gap_start = end;
                gap_end = end;
                newlines = 0;
            }
        }
    }

    // The trailing gap: whatever stands after the last token.
    if let Some(last) = comments.last_mut() {
        last.ends_line = true;
    }
    gaps.push(Gap {
        start: gap_start,
        end: gap_end,
        newlines,
        comments,
        at_end: true,
    });

    Stream {
        source,
        tokens,
        gaps,
    }
}

/// The whitespace *before* a comment, and after the previous item: let the layout know whether the comment starts
/// a line. (Kept as a method so that `Comment`'s fields stay the only data.)
impl<'a> Comment<'a> {
    pub fn text(&self) -> &'a str {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gap_carries_its_comments_in_order_and_whether_they_start_a_line() {
        let source = "int x; // trailing\n// own line\nint y;\n";
        let stream = stream(source);
        assert_eq!(stream.tokens.len(), 6, "int x ; int y ;  -> 6 tokens");
        // `int x ; int y ;` — the two comments stand before the second `int`, which is token index 3.
        let gap = &stream.gaps[3];
        assert_eq!(gap.comments.len(), 2);
        assert!(!gap.comments[0].own_line, "the first comment trails `int x;`");
        assert!(gap.comments[0].ends_line);
        assert!(gap.comments[1].own_line);
        assert!(gap.comments[1].ends_line);
        assert_eq!(gap.comments[0].text(), "// trailing");
    }

    #[test]
    fn the_last_gap_is_the_end_of_the_file() {
        let stream = stream("int x;\n");
        assert!(stream.gaps.last().unwrap().at_end);
        assert_eq!(stream.gaps.len(), stream.tokens.len() + 1);
    }

    #[test]
    fn a_line_continuation_is_not_a_newline() {
        let stream = stream("#define A \\\n  1\n");
        let first = &stream.tokens[0];
        assert_eq!(first.text, "#");
        // The `\` + newline is trivia, and no comment occupies it.
        assert!(stream.gaps[1].comments.is_empty());
    }
}
