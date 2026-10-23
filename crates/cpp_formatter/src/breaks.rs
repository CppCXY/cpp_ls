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

// ---------------------------------------------------------------------------------------------------------------
// The break search (P2's second half): where to split one logical line so that it fits.
// ---------------------------------------------------------------------------------------------------------------

/// The penalties of one style that the search uses (`docs/cpp-format.md` §十 P2).
#[derive(Debug, Clone, Copy)]
pub struct Penalties {
    /// Per column a line runs past the limit — clang-format's `PenaltyExcessCharacter`.
    pub excess_character: i64,
    /// A break before the first parameter of a call — `PenaltyBreakBeforeFirstCallParameter`.
    pub break_before_first_parameter: i64,
    /// A break at an assignment — `PenaltyBreakAssignment`.
    pub break_assignment: i64,
    /// Any other break.
    pub plain: i64,
}

impl Penalties {
    /// The weights of a style, read from its own keys.
    pub fn of(style: &crate::config::Style) -> Penalties {
        use crate::options::StyleOption;
        Penalties {
            excess_character: style.number(StyleOption::PenaltyExcessCharacter).max(1),
            break_before_first_parameter: style
                .number(StyleOption::PenaltyBreakBeforeFirstCallParameter),
            break_assignment: style.number(StyleOption::PenaltyBreakAssignment),
            plain: 1,
        }
    }
}

/// **Where to break one logical line** so that it fits the limit, as token indices that start a new line.
///
/// A dynamic program over the *breakable* positions of the line: breaking after a comma, before a binary operator,
/// before a `?`, after an opening bracket, or before a member access. The cost of a layout is the excess of every
/// over-long piece times [`Penalties::excess_character`] plus a fixed cost per break, which is clang-format's model
/// with the weights it actually uses — the search is exact for the candidate set, and the candidate set is the part
/// this module deliberately keeps small.
///
/// `None` when there is nothing to do (the line fits) and `Some(vec![])` never happens: a caller that gets `Some` can
/// print the line in pieces, one per returned index plus the rest.
pub fn break_line(
    stream: &Stream<'_>,
    roles: &crate::roles::Roles,
    style: &crate::config::Style,
    line: &Range<usize>,
    start_column: usize,
) -> Option<Vec<usize>> {
    let limit = style.count(crate::options::StyleOption::ColumnLimit);
    if limit == 0 {
        return None;
    }
    let penalties = Penalties::of(style);
    let continuation = style.count(crate::options::StyleOption::ContinuationIndentWidth);

    // **The candidate positions**: the first token of the line, plus every token a break may precede.
    let mut positions: Vec<usize> = vec![line.start];
    for index in line.start + 1..line.end {
        if is_breakable(stream, index) {
            positions.push(index);
        }
    }
    positions.push(line.end);
    if positions.len() <= 2 {
        return None;
    }

    // The width of a piece `a..b` when printed after the previous piece's end.
    let width = |a: usize, b: usize, indent: usize| -> usize {
        let mut column = indent;
        for index in a..b {
            if index > a {
                column += crate::space::between(stream, index, roles, style);
            }
            column += stream.tokens[index].text.chars().count();
        }
        column
    };

    // `best[i]` = the cheapest way to lay out `positions[0..=i]`, with the last piece ending at `positions[i]`.
    let count = positions.len();
    let mut best = vec![i64::MAX; count];
    let mut from = vec![0usize; count];
    best[0] = 0;
    for end in 1..count {
        for start in 0..end {
            if best[start] == i64::MAX {
                continue;
            }
            // **A piece after the first one starts at the continuation column**, not at the statement`s: charging the
            // statement column made the search believe pieces fitted when they did not.
            let indent = if start == 0 { start_column } else { start_column + continuation };
            let piece = width(positions[start], positions[end], indent);
            let excess = piece.saturating_sub(limit) as i64 * penalties.excess_character;
            let break_cost = if start == 0 { 0 } else { break_at(stream, positions[start], &penalties) };
            let cost = best[start] + excess + break_cost;
            if cost < best[end] {
                best[end] = cost;
                from[end] = start;
            }
        }
    }

    // Reconstruct: every chosen position except the first and the end is a line break.
    let mut chosen = Vec::new();
    let mut cursor = count - 1;
    while cursor > 0 {
        let previous = from[cursor];
        if previous > 0 {
            chosen.push(positions[previous]);
        }
        cursor = previous;
    }
    chosen.reverse();
    if chosen.is_empty() { None } else { Some(chosen) }
}

/// May a break go before the token at `index`?
pub fn is_breakable(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let previous = stream.tokens[index - 1].kind;
    let current = stream.tokens[index].kind;
    match current {
        // After a comma: the classic argument break.
        _ if previous == Comma => true,
        // Before a binary operator (`a + b` broken before the `+`).
        Plus | Minus | Star | Slash | Percent | Ampersand | Pipe | Caret | Less | Greater | LessEqual
        | GreaterEqual | Equal | NotEqual | LogicalAnd | LogicalOr | LeftShift | RightShift | Question => true,
        // Before a member access and after an opening bracket.
        Dot | Arrow => true,
        _ if matches!(previous, LeftParen | LeftBracket) => true,
        _ => false,
    }
}

/// **The breaks an over-long *source line* needs.** The pass keeps the source's line breaks, so a piece that is
/// already on its own line is only too long if that line is: running the search on such a piece adds breaks where they
/// help, and leaves every other piece alone.
pub fn break_over_long_pieces(
    stream: &Stream<'_>,
    roles: &crate::roles::Roles,
    style: &crate::config::Style,
    line: &Range<usize>,
    blocks: &[Option<usize>],
) -> Vec<usize> {
    let limit = style.count(crate::options::StyleOption::ColumnLimit);
    let mut breaks = Vec::new();
    let mut start = line.start;
    for index in line.start..line.end {
        let ends_a_line = index + 1 == line.end || stream.gaps[index + 1].newlines > 0;
        if !ends_a_line {
            continue;
        }
        let column = blocks.get(start).copied().flatten().unwrap_or(0);
        let mut width = column;
        for inside in start..=index {
            if inside > start {
                width += crate::space::between(stream, inside, roles, style);
            }
            width += stream.tokens[inside].text.chars().count();
        }
        if width > limit
            && let Some(found) = break_line(stream, roles, style, &(start..index + 1), column)
        {
            breaks.extend(found);
        }
        start = index + 1;
    }
    breaks
}
/// The fixed cost of breaking before the token at `index`.
fn break_at(stream: &Stream<'_>, index: usize, penalties: &Penalties) -> i64 {
    use CppTokenKind::*;
    match stream.tokens[index].kind {
        Assign => penalties.break_assignment,
        _ if stream.tokens[index - 1].kind == LeftParen => penalties.break_before_first_parameter,
        _ => penalties.plain,
    }
}

#[cfg(test)]
mod break_tests {
    use super::*;
    use crate::config::Style;

    fn broken(source: &str, limit: &str) -> Option<Vec<String>> {
        let mut style = Style::default_style();
        style.set_from_text("ColumnLimit", limit);
        let stream = crate::tokens::stream(source);
        let roles = crate::roles::of(source);
        let lines = logical_lines(&stream);
        let line = lines.first().expect("a line").clone();
        let breaks = break_line(&stream, &roles, &style, &line, 0)?;
        // Print the pieces, so that a test can read them.
        let mut pieces = Vec::new();
        let mut start = line.start;
        for at in breaks.iter().copied().chain(std::iter::once(line.end)) {
            let mut text = String::new();
            for index in start..at {
                if index > start {
                    text.push(' ');
                }
                text.push_str(stream.tokens[index].text);
            }
            pieces.push(text);
            start = at;
        }
        Some(pieces)
    }

    #[test]
    fn a_line_that_fits_is_not_broken() {
        assert_eq!(broken("int a = 1;", "80"), None);
    }

    #[test]
    fn a_long_argument_list_breaks_after_the_commas() {
        let pieces = broken("f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc);", "20").expect("broken");
        assert!(pieces.len() > 1, "{pieces:?}");
        for piece in &pieces {
            assert!(piece.chars().count() <= 20, "{piece:?} in {pieces:?}");
        }
    }

    #[test]
    fn a_long_sum_breaks_before_the_operator() {
        let pieces = broken("int x = aaaaaaaaaa + bbbbbbbbbb + cccccccccc;", "25").expect("broken");
        assert!(pieces.iter().any(|piece| piece.starts_with('+')), "{pieces:?}");
    }

    #[test]
    fn the_search_is_deterministic() {
        let first = broken("f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc);", "20");
        let second = broken("f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc);", "20");
        assert_eq!(first, second);
    }
}