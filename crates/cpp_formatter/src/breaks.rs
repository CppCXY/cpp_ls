//! **Logical lines: the unit a line breaker works on.**
//!
//! Everything up to here decides *columns* — where a token sits once the breaks are known. The
//! breaker needs the other half: which tokens belong together, i.e. clang-format's `UnwrappedLine`.
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
    logical_lines_with(stream, None)
}

/// **The logical lines, with an initializer`s braces kept inside their statement**: a brace that opens an initializer
/// does not end a logical line, so `int v[] = {1, 2};` can be put on one line by the join pass — clang-format writes it
/// that way when it fits. Without the roles (a caller that has none) every brace splits, which is the old behaviour.
pub fn logical_lines_with(
    stream: &Stream<'_>,
    roles: Option<&crate::roles::Roles>,
) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut parens = 0isize;
    // **A preprocessor directive is one logical line, splices and all**: its body may hold `;`, `{` and `}`, and
    // splitting there glued the macro's last line to the declaration after it (`lu_byte tt_ typedef struct TValue {`,
    // measured on Lua 5.5's `lobject.h`). The directive ends at the first newline in the gap after a token.
    let mut in_directive = false;
    // The `{` of each open brace, so an initializer's closer can be told from a block's (only with roles).
    let mut braces: Vec<usize> = Vec::new();

    for index in 0..stream.tokens.len() {
        let kind = stream.tokens[index].kind;
        if in_directive {
            // The newline **before** this token ends the directive, so this token starts a new logical line.
            if stream.gaps[index].newlines > 0 {
                in_directive = false;
                lines.push(start..index);
                start = index;
            } else {
                continue;
            }
        }
        if kind == CppTokenKind::Hash && (index == 0 || stream.gaps[index].newlines > 0) {
            in_directive = true;
            continue;
        }
        // **A bare macro invocation is a line of its own**: `NLOHMANN_JSON_NAMESPACE_BEGIN` has no `;`, and without
        // this the logical line ran into the declaration after it and the reflow joined them into
        // `NLOHMANN_JSON_NAMESPACE_BEGIN namespace detail {` (measured on nlohmann json). The test is the shape
        // clang-format uses for a macro: an all-capital identifier that ends its line.
        if kind == CppTokenKind::Identifier
            && is_a_macro_name(stream.tokens[index].text)
            && stream
                .gaps
                .get(index + 1)
                .is_some_and(|gap| gap.newlines > 0)
            && index + 1 > start
        {
            lines.push(start..index + 1);
            start = index + 1;
            continue;
        }
        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket => parens += 1,
            CppTokenKind::RightParen | CppTokenKind::RightBracket => parens -= 1,
            _ => {}
        }
        // **A block brace is a line of its own**: the code before it ends the previous line, and the brace ends this
        // one. **An initializer's brace stays inside the statement** when the roles are known, which is what lets the
        // join pass put a fitting `int v[] = {1, 2};` on one line (clang-format does).
        let an_initializer = match (kind, roles) {
            (CppTokenKind::LeftBrace, Some(roles)) => {
                braces.push(index);
                roles.init_brace.contains(&stream.tokens[index].start)
            }
            (CppTokenKind::RightBrace, Some(roles)) => braces
                .pop()
                .is_some_and(|open| roles.init_brace.contains(&stream.tokens[open].start)),
            _ => false,
        };
        if matches!(kind, CppTokenKind::LeftBrace | CppTokenKind::RightBrace)
            && parens == 0
            && !an_initializer
        {
            if start < index {
                lines.push(start..index);
            }
            // **A `;` written directly after a closing brace belongs to the brace's line.** It is what a class
            // definition (`};`), a lambda and a compound statement followed by an empty statement write, and
            // splitting it put `;` alone on the next line: measured on neovim's `TEXTLOCK_WRAP({ … });`, whose
            // expansion showed `}` / `;` because the macro's own body writes `code;` and the argument is a block.
            let through = if kind == CppTokenKind::RightBrace
                && stream
                    .tokens
                    .get(index + 1)
                    .is_some_and(|next| next.kind == CppTokenKind::Semicolon)
            {
                index + 2
            } else {
                index + 1
            };
            lines.push(index..through);
            start = through;
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

/// Is this identifier shaped like a macro name (`NLOHMANN_JSON_NAMESPACE_BEGIN`, `QT_BEGIN_NAMESPACE`)?
fn is_a_macro_name(text: &str) -> bool {
    text.chars().count() >= 2
        && text.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Is the colon at `index` the end of a `case`/`default`/access label?
pub(crate) fn is_a_label(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CaseKeyword | DefaultKeyword | PublicKeyword | PrivateKeyword | ProtectedKeyword => {
                return true;
            }
            Semicolon | LeftBrace | RightBrace | Question => return false,
            _ => {}
        }
    }
    false
}

/// **Is the colon at `index` the end of a `case`/`default` label?** — the variant of [`is_a_label`] that leaves an access
/// specifier out: `public:` is placed by its own rules, and wrapping the member after it broke a constructor fixture.
pub(crate) fn is_a_case_label(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CaseKeyword | DefaultKeyword => return true,
            PublicKeyword | PrivateKeyword | ProtectedKeyword | Semicolon | LeftBrace
            | RightBrace | Question => return false,
            _ => {}
        }
    }
    false
}

/// Does the `)` at `index` close the condition of an `if`/`for`/`while`/`switch` whose body starts on the next line?
pub(crate) fn starts_a_control_statement(stream: &Stream<'_>, index: usize) -> bool {
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
    // **A `while` after a `}` is the tail of a `do { … } while (…)`**, not a head that opens a body: it ends at the
    // `;` that follows, and treating it as a body head indented the next line by a level (measured on a macro fixture
    // whose `do { … } while (0)` was followed by `namespace ns {`).
    if stream.tokens[open - 1].kind == WhileKeyword
        && open >= 2
        && stream.tokens[open - 2].kind == RightBrace
    {
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
        assert_eq!(texts("if (x)\nreturn;\n"), vec!["if ( x )", "return ;"]);
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
    // **Room the last piece must leave for something that is not a token** — a trailing comment. clang-format breaks
    // a declaration that would fit because the comment after it would not.
    extra_width: usize,
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
        if is_breakable(stream, roles, style, index) {
            positions.push(index);
        }
    }
    positions.push(line.end);
    if positions.len() <= 2 {
        return None;
    }

    // **The bracket each token sits in, and the column it would have in the joined line.** `AlignAfterOpenBracket:
    // Align` puts a continuation inside a bracket under the bracket`s content — the column only the joined layout can
    // say — and a break immediately after the bracket falls back to a continuation indent. Without this the search
    // only knew one continuation column, which is why its breaks did not look like clang-format`s (measured on
    // `AlignAnalyzer.cpp`: clang breaks after `!=` and aligns the operand, the search broke elsewhere).
    let mut enclosing: Vec<Option<usize>> = vec![None; stream.tokens.len()];
    let mut joined_column: Vec<usize> = vec![0; stream.tokens.len()];
    {
        let mut open: Vec<usize> = Vec::new();
        let mut column = start_column;
        for index in line.start..line.end {
            if index > line.start {
                column += crate::space::between(stream, index, roles, style);
            }
            joined_column[index] = column;
            enclosing[index] = open.last().copied();
            match stream.tokens[index].kind {
                CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                    open.push(index)
                }
                CppTokenKind::RightParen
                | CppTokenKind::RightBracket
                | CppTokenKind::RightBrace => {
                    open.pop();
                }
                _ => {}
            }
            column += stream.tokens[index].text.chars().count();
        }
    }
    let aligns = matches!(
        style.text(crate::options::StyleOption::AlignAfterOpenBracket),
        "Align" | "AlwaysBreak"
    ) || style.flag(crate::options::StyleOption::AlignAfterOpenBracket);
    // The column a piece starting at `position` gets.
    let indent_at = |position: usize, first: bool| -> usize {
        match enclosing[position] {
            Some(open) if aligns && open + 1 < position => joined_column[open + 1],
            Some(_) => start_column + continuation,
            None if first => start_column,
            None => start_column + continuation,
        }
    };

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
            // **A piece after the first one starts at its bracket`s content column** when the style aligns inside
            // brackets, and at the continuation column otherwise — charging the statement column made the search
            // believe pieces fitted when they did not.
            let indent = indent_at(positions[start], start == 0);
            let piece = width(positions[start], positions[end], indent)
                + if positions[end] == line.end {
                    extra_width
                } else {
                    0
                };
            let excess = piece.saturating_sub(limit) as i64 * penalties.excess_character;
            let break_cost = if start == 0 {
                0
            } else {
                break_at(stream, positions[start], &penalties)
            };
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
    if chosen.is_empty() {
        None
    } else {
        Some(chosen)
    }
}

/// **The source with a line break before every logical line** — clang-format's layout, as text a formatter can then
/// indent.
///
/// [`crate::format`] deliberately keeps the author's line structure: a statement written on the line of the one before
/// it stays there. That is the right contract for a *formatting* request on a file someone is editing, and the wrong
/// one for text with no line structure at all — a macro's expansion, a signature rebuilt from facts — where the reader
/// wants the shape clang-format would give it.
///
/// The token bytes are untouched: the only regions rewritten are the whitespace-only gaps between two tokens, and a gap
/// with a comment, a directive or a line already starting there is left exactly as it was. A newline inside a
/// directive would break the macro it belongs to, which is why this scan tracks them the way `render` does.
pub fn laid_out(source: &str, style: &crate::config::Style) -> String {
    let stream = crate::tokens::stream(source);
    let lines = logical_lines(&stream);
    let mut starts: std::collections::HashSet<usize> =
        lines.iter().skip(1).map(|line| line.start).collect();
    let ending = crate::tokens::line_ending(source);
    let mut out = String::with_capacity(source.len() + starts.len() * ending.len());
    let mut in_directive = false;
    // **The brace style is a decision of this pass**, and it is the one clang-format makes: `Attach` keeps `{` on the
    // line of what it opens, `} else {` and `} while (0);` stay on the line of the `}` they follow, and every other
    // style starts a line at the brace. `format` alone cannot do it — a file whose braces the author placed keeps
    // them — which is why the layout step has to choose.
    let attaches = style.text(crate::options::StyleOption::BreakBeforeBraces) == "Attach";
    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        let text = source.get(gap.start..gap.end).unwrap_or("");
        if stream.tokens[index].text == "#" && (index == 0 || gap.newlines > 0) {
            in_directive = true;
        }
        let current = &stream.tokens[index];
        let previous = stream.tokens.get(index.wrapping_sub(1));
        let attaches_to_the_line_before = attaches
            && (current.kind == cpp_parser::CppTokenKind::LeftBrace
                || (previous.is_some_and(|previous| {
                    previous.kind == cpp_parser::CppTokenKind::RightBrace
                }) && matches!(current.text, "else" | "catch" | "while")));
        let needs_a_break = index > 0
            && !in_directive
            && !attaches_to_the_line_before
            && starts.remove(&index)
            && gap.newlines == 0
            && gap.comments.is_empty()
            && !gap.at_end;
        if needs_a_break {
            out.push_str(ending);
        } else {
            out.push_str(text);
        }
        out.push_str(stream.tokens[index].text);
        if in_directive
            && stream
                .gaps
                .get(index + 1)
                .is_some_and(|next| next.newlines > 0)
        {
            in_directive = false;
        }
    }
    if let Some(trailing) = stream.gaps.last() {
        out.push_str(source.get(trailing.start..trailing.end).unwrap_or(""));
    }
    out
}

/// May a break go before the token at `index`?
pub fn is_breakable(
    stream: &Stream<'_>,
    roles: &crate::roles::Roles,
    style: &crate::config::Style,
    index: usize,
) -> bool {
    use CppTokenKind::*;
    let previous = stream.tokens[index - 1].kind;
    let current = stream.tokens[index].kind;
    let start = stream.tokens[index].start;
    let previous_start = stream.tokens[index - 1].start;
    // After a comma: the classic argument break.
    if previous == Comma {
        return true;
    }
    // **`BreakBeforeBinaryOperators` decides which side of an operator the break goes on.** `All` breaks before it
    // (`a + b` -> `a` / `+ b`); `None` — LLVM's and Google's value — breaks **after** it (`a +` / `b`), which is the
    // shape clang-format's own output has everywhere. Reading only "before a binary operator" put our breaks on the
    // wrong side of every operator: measured on `AlignAnalyzer.cpp`, clang-format writes
    // `… align_continuous_assign_statement !=` then `ContinuousAlign::None) {`.
    let break_before = match style.text(crate::options::StyleOption::BreakBeforeBinaryOperators) {
        "All" => true,
        "NonAssignment" => crate::layout::is_a_binary_operator(current) && current == Assign,
        _ => false,
    };
    if crate::layout::is_a_binary_operator(current) && break_before {
        return !roles.is_declarator(start) && !roles.is_template_angle(start);
    }
    if crate::layout::is_a_binary_operator(previous)
        && !break_before
        && !roles.is_declarator(previous_start)
        && !roles.is_template_angle(previous_start)
    {
        return true;
    }
    // **Before a declarator's `*`/`&` or a template's angle is never a break**: `LuaStyle &style` is a type and its
    // declarator, and `std::vector<int>` is one name (both measured: the alternative broke inside a name).
    if roles.is_declarator(start) || roles.is_template_angle(start) {
        return false;
    }
    // **A break after a template argument list's `>` was tried and reverted**: it is what clang-format does for
    // `std::unique_ptr<frame[]>` then `heap{};` (measured), but adding it as a candidate moved the whole-file columns the
    // wrong way by a lot — nlohmann json 20 426 → 20 536 `LLVM` and 20 512 → 20 580 `Google`, EmmyLuaCodeStyle 1 434 →
    // 1 442 — because the search then breaks after every template closer where the author's line already fitted. See
    // `crates/cpp_formatter/tests/suite/against_clang_format.rs` classifies the differences that remain.
    match current {
        // Before a member access and after an opening bracket.
        Dot | Arrow => true,
        // **Never between an empty pair of brackets**: `std::unique_ptr<frame[]>` is one type, and breaking before the
        // `]` wrote `frame[` then `]>` — a shape clang-format never produces (measured on nlohmann json's
        // `json_sax.hpp`, where the member's long `// NOLINT(…)` comment is what makes the line overflow).
        _ if previous == LeftBracket && current == RightBracket => false,
        _ if matches!(previous, LeftParen | LeftBracket) => true,
        // A conditional's `?`/`:` still breaks before itself.
        Question => true,
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
            && let Some(found) = break_line(stream, roles, style, &(start..index + 1), column, 0)
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
        broken_with(style, source)
    }

    /// The same with the style spelled by the case — an option the break side depends on.
    fn broken_with(style: Style, source: &str) -> Option<Vec<String>> {
        let stream = crate::tokens::stream(source);
        let roles = crate::roles::of(source);
        let lines = logical_lines(&stream);
        let line = lines.first().expect("a line").clone();
        let breaks = break_line(&stream, &roles, &style, &line, 0, 0)?;
        // Print the pieces, so that a test can read them.
        let mut pieces = Vec::new();
        let mut start = line.start;
        for at in breaks.iter().copied().chain(std::iter::once(line.end)) {
            let mut text = String::new();
            for (offset, token) in stream.tokens[start..at].iter().enumerate() {
                if offset > 0 {
                    text.push(' ');
                }
                text.push_str(token.text);
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
    fn a_long_sum_breaks_after_the_operator_under_none() {
        // **`BreakBeforeBinaryOperators: None` is LLVM's and Google's value**: the break goes after the operator
        // (`aaaaaaaaaa +` / `bbbbbbbbbb +`), which is the shape clang-format's own output has. The old expectation
        // here was the `All` shape, and reading only "before a binary operator" put every break on the wrong side.
        let pieces = broken("int x = aaaaaaaaaa + bbbbbbbbbb + cccccccccc;", "25").expect("broken");
        assert!(
            pieces.iter().any(|piece| piece.trim_end().ends_with('+')),
            "{pieces:?}"
        );
        assert!(
            pieces
                .iter()
                .skip(1)
                .all(|piece| !piece.trim_start().starts_with('+')),
            "{pieces:?}"
        );
    }

    #[test]
    fn a_long_sum_breaks_before_the_operator_under_all() {
        let mut style = Style::default_style();
        style.set_from_text("BreakBeforeBinaryOperators", "All");
        style.set_from_text("ColumnLimit", "25");
        let pieces =
            broken_with(style, "int x = aaaaaaaaaa + bbbbbbbbbb + cccccccccc;").expect("broken");
        assert!(
            pieces
                .iter()
                .skip(1)
                .any(|piece| piece.trim_start().starts_with('+')),
            "{pieces:?}"
        );
    }

    #[test]
    fn the_search_is_deterministic() {
        let first = broken("f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc);", "20");
        let second = broken("f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc);", "20");
        assert_eq!(first, second);
    }
}
