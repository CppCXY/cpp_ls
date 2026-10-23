//! **Logical lines: the unit a line breaker works on.**
//!
//! P2 of `docs/cpp-format.md` §十. Everything up to here decides *columns* — where a token sits once the breaks are
//! known. The breaker needs the other half: which tokens belong together, i.e. clang-format's `UnwrappedLine`.
//!
//! # The subset this module implements
//!
//! ```text
//! end after `;`        a statement ends there (at parenthesis depth 0)
//! end after `{`/`}`     a block's brace ends its line (`void f() {`, `}`)
//! end after a label    `case X:` and `public:` end at their colon
//! end after `)` of a control statement followed by something else   (`if (x)` then the statement on the next line)
//! ```
//!
//! This is deliberately smaller than clang-format's parser: what the rest of the formatter needs is "how much text
//! would one line hold", and an over-long logical line is over-long under any of the finer divisions too. The tests
//! pin each rule, so extending the set later is a matter of adding a case.
//!
//! # What it is for
//!
//! `examples/column_audit.rs` measures the P2 gap: with `ColumnLimit: 80` we print 113 lines over the limit where
//! clang-format prints none. This module turns "line" into "logical line", which is what the breaker will break and
//! what a measurement should count.

use std::ops::Range;

use cpp_parser::CppTokenKind;

use crate::tokens::Stream;

/// **The logical lines of a token stream**, as ranges of token indices.
///
/// Every token belongs to exactly one range (the ranges are contiguous and cover `0..tokens.len()`), so a caller can
/// sum widths without checking for gaps.
pub fn logical_lines(stream: &Stream<'_>) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut parens = 0isize;

    for index in 0..stream.tokens.len() {
        let kind = stream.tokens[index].kind;
        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket => parens += 1,
            CppTokenKind::RightParen | CppTokenKind::RightBracket => parens -= 1,
            _ => {}
        }
        // **A brace is a line of its own**: the code before it ends the previous line, and the brace ends this one.
        if matches!(kind, CppTokenKind::LeftBrace | CppTokenKind::RightBrace) && parens == 0 {
            if start < index {
                lines.push(start..index);
            }
            lines.push(index..index + 1);
            start = index + 1;
            continue;
        }
        let ends = match kind {
            CppTokenKind::Semicolon if parens == 0 => true,
            // A label's colon ends the line (`case 1:`, `public:`) — but not a ternary's colon, which never sits at
            // parenthesis depth 0 inside a label position. A struct initializer's `:` is inside a brace.
            CppTokenKind::Colon if parens == 0 => is_a_label(stream, index),
            // `if (x)` followed by a statement on the next line: the closing parenthesis ends the head.
            CppTokenKind::RightParen if parens == 0 => starts_a_control_statement(stream, index),
            _ => false,
        };
        if ends && index + 1 > start {
            lines.push(start..index + 1);
            start = index + 1;
        }
    }
    if start < stream.tokens.len() {
        lines.push(start..stream.tokens.len());
    }
    lines
}

/// Is the colon at `index` the end of a `case`/`default`/access label?
fn is_a_label(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CaseKeyword | DefaultKeyword | PublicKeyword | PrivateKeyword | ProtectedKeyword => return true,
            Semicolon | LeftBrace | RightBrace | Question => return false,
            _ => {}
        }
    }
    false
}

/// Does the `)` at `index` close the condition of an `if`/`for`/`while`/`switch` whose body starts on the next line?
fn starts_a_control_statement(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    // Find the `(` that matches this `)`.
    let mut depth = 0isize;
    let mut cursor = index;
    let mut open: Option<usize> = Option::None;
    loop {
        match stream.tokens[cursor].kind {
            RightParen => depth += 1,
            LeftParen => {
                depth -= 1;
                if depth == 0 {
                    open = Some(cursor);
                    break;
                }
            }
            _ => {}
        }
        if cursor == 0 {
            break;
        }
        cursor -= 1;
    }
    let Some(open) = open else {
        return false;
    };
    if open == 0 {
        return false;
    }
    matches!(
        stream.tokens[open - 1].kind,
        IfKeyword | ForKeyword | WhileKeyword | SwitchKeyword | CatchKeyword
    ) && stream
        .gaps
        .get(index + 1)
        .is_some_and(|gap| gap.newlines > 0 || gap.at_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::stream;

    fn texts(source: &str) -> Vec<String> {
        let stream = stream(source);
        logical_lines(&stream)
            .into_iter()
            .map(|line| {
                stream.tokens[line]
                    .iter()
                    .map(|token| token.text)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn a_statement_ends_at_its_semicolon() {
        assert_eq!(
            texts("int a = 1;\nint b = 2;\n"),
            vec!["int a = 1 ;", "int b = 2 ;"]
        );
    }

    #[test]
    fn a_block_brace_ends_its_own_line() {
        assert_eq!(
            texts("void f()\n{\nreturn;\n}\n"),
            vec!["void f ( )", "{", "return ;", "}"]
        );
    }

    #[test]
    fn a_case_label_ends_at_its_colon() {
        assert_eq!(
            texts("switch (x) {\ncase 1:\nbreak;\n}\n"),
            vec!["switch ( x )", "{", "case 1 :", "break ;", "}"]
        );
    }

    #[test]
    fn a_control_head_ends_at_its_parenthesis_when_the_body_is_on_the_next_line() {
        assert_eq!(
            texts("if (x)\nreturn;\n"),
            vec!["if ( x )", "return ;"]
        );
    }

    #[test]
    fn every_token_belongs_to_exactly_one_line() {
        let source = "void f()\n{\nif (x) {\nreturn;\n}\n}\n";
        let stream = stream(source);
        let lines = logical_lines(&stream);
        let covered: usize = lines.iter().map(|line| line.len()).sum();
        assert_eq!(covered, stream.tokens.len(), "{lines:?}");
    }
}