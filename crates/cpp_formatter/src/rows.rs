//! **`AlignArrayOfStructures: Left` — the columns of a braced table.**
//!
//! P1 of `docs/cpp-format.md` §十, the last aligner the corpus asks for. A table is a run of adjacent lines that each
//! open with `{`; its cells are the tokens between top-level commas. clang-format pads every cell to the widest one in
//! its column, so the commas and closing braces line up:
//!
//! ```text
//! {"and",      TK_AND     },
//! {"function", TK_FUNCTION},     ← the widest first cell and the widest second one set the columns
//! {"::",       TK_DBCOLON }      ← the last row may omit the trailing comma and is still a row
//! ```
//!
//! The constants were read off `LuaLexer.cpp`, where clang-format produced the file: the second column starts at
//! `1 + widest_first + 2` (brace, widest first cell, comma and one space), and the closing brace follows the padded
//! last cell with no space of its own (`start + widest_last`).

use crate::config::Style;
use crate::options::StyleOption;
use crate::roles::Roles;
use crate::space;
use crate::tokens::Stream;
use cpp_parser::CppTokenKind;

/// Extra spaces to insert before each token (indexed like the stream's tokens) so that a table's columns line up.
pub fn paddings(stream: &Stream<'_>, roles: &Roles, style: &Style) -> Vec<usize> {
    let mut paddings = vec![0usize; stream.tokens.len()];
    if style.text(StyleOption::AlignArrayOfStructures) != "Left" {
        return paddings;
    }

    let all = rows(stream);
    let mut start = 0usize;
    while start < all.len() {
        let mut end = start + 1;
        while end < all.len()
            && line_of(stream, all[end].open) == line_of(stream, all[end - 1].open) + 1
        {
            end += 1;
        }
        if end - start >= 2 {
            align_run(stream, roles, style, &all[start..end], &mut paddings);
        }
        start = end;
    }

    paddings
}

/// The line a token starts on.
fn line_of(stream: &Stream<'_>, index: usize) -> usize {
    stream.gaps[..=index].iter().map(|gap| gap.newlines).sum()
}

/// One row of a table: the `{` that opens it and the `}` that closes it, both token indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    open: usize,
    close: usize,
}

/// The rows of the stream: a `{` that starts a line, its matching `}`, and then either `,` or the end of the line.
fn rows(stream: &Stream<'_>) -> Vec<Row> {
    let mut rows = Vec::new();
    for index in 0..stream.tokens.len() {
        let begins = index == 0 || stream.gaps[index].newlines > 0;
        if !begins || stream.tokens[index].kind != CppTokenKind::LeftBrace {
            continue;
        }
        let mut depth = 0isize;
        for cursor in index..stream.tokens.len() {
            match stream.tokens[cursor].kind {
                CppTokenKind::LeftBrace => depth += 1,
                CppTokenKind::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        // The line ends after `}` or after `},`.
                        let after = cursor + 1;
                        let comma = stream
                            .tokens
                            .get(after)
                            .is_some_and(|token| token.kind == CppTokenKind::Comma);
                        let end_of_line = |token_index: usize| {
                            token_index >= stream.tokens.len()
                                || stream
                                    .gaps
                                    .get(token_index)
                                    .is_some_and(|gap| gap.newlines > 0)
                        };
                        let ends = if comma {
                            end_of_line(after + 1)
                        } else {
                            end_of_line(after)
                        };
                        if ends {
                            rows.push(Row {
                                open: index,
                                close: cursor,
                            });
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    rows
}

/// One row, measured: its cells (token indices) and the natural column of every token it contains.
struct Measured {
    cells: Vec<Vec<usize>>,
    natural: Vec<usize>,
    close: usize,
}

/// **Pad one run's columns to the widest cell of each.**
fn align_run(
    stream: &Stream<'_>,
    roles: &Roles,
    style: &Style,
    run: &[Row],
    paddings: &mut [usize],
) {
    let mut measured: Vec<Measured> = Vec::new();
    for row in run {
        let mut natural = vec![0usize; stream.tokens.len()];
        let mut column = 0usize;
        for (offset, column_at) in natural[row.open..=row.close].iter_mut().enumerate() {
            let index = row.open + offset;
            if index > row.open {
                column += space::between(stream, index, roles, style);
            }
            *column_at = column;
            column += stream.tokens[index].text.chars().count();
        }
        let mut cells: Vec<Vec<usize>> = vec![Vec::new()];
        let mut depth = 0isize;
        for index in row.open + 1..row.close {
            match stream.tokens[index].kind {
                CppTokenKind::LeftBrace | CppTokenKind::LeftParen | CppTokenKind::LeftBracket => {
                    depth += 1
                }
                CppTokenKind::RightBrace
                | CppTokenKind::RightParen
                | CppTokenKind::RightBracket => depth -= 1,
                CppTokenKind::Comma if depth == 0 => {
                    cells.push(Vec::new());
                    continue;
                }
                _ => {}
            }
            if let Some(cell) = cells.last_mut() {
                cell.push(index);
            }
        }
        if cells.last().is_some_and(Vec::is_empty) {
            cells.pop();
        }
        measured.push(Measured {
            cells,
            natural,
            close: row.close,
        });
    }

    let columns = measured
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(0);
    if columns < 2 {
        return;
    }
    let mut widest = vec![0usize; columns];
    for row in &measured {
        for (column, cell) in row.cells.iter().enumerate() {
            if let (Some(first), Some(last)) = (cell.first(), cell.last()) {
                let width = row.natural[*last] + stream.tokens[*last].text.chars().count()
                    - row.natural[*first];
                widest[column] = widest[column].max(width);
            }
        }
    }
    let mut starts = vec![0usize; columns];
    starts[0] = 1;
    for column in 1..columns {
        starts[column] = starts[column - 1] + widest[column - 1] + 2;
    }
    let close_target = starts[columns - 1] + widest[columns - 1];

    for row in &measured {
        for (column, cell) in row.cells.iter().enumerate() {
            if let Some(first) = cell.first()
                && column < starts.len()
            {
                paddings[*first] += starts[column].saturating_sub(row.natural[*first]);
            }
        }
        // The closing brace moves with the last cell: its padding is measured from where that cell *ends up*.
        let last = row.cells.len().saturating_sub(1);
        let shift = row
            .cells
            .get(last)
            .and_then(|cell| cell.first())
            .map_or(0, |first| {
                starts
                    .get(last)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(row.natural[*first])
            });
        paddings[row.close] += close_target.saturating_sub(row.natural[row.close] + shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;

    fn padded(source: &str) -> Vec<usize> {
        let mut style = Style::default_style();
        style.set_from_text("AlignArrayOfStructures", "Left");
        let stream = crate::tokens::stream(source);
        let roles = crate::roles::of(source);
        paddings(&stream, &roles, &style)
    }

    #[test]
    fn the_rows_of_a_table_include_the_last_one_without_a_comma() {
        let stream = crate::tokens::stream(
            "int t[] = {\n  {\"a\", 1},\n  {\"bb\", 2},\n  {\"ccc\", 3}\n};\n",
        );
        let found = rows(&stream);
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn a_block_that_starts_a_line_gets_no_padding() {
        // A bare block may look like a one-cell row (a `{` at a line start and its matching `}`); with no top-level
        // comma there is no column to align, so it must come out untouched.
        let found = padded("void f()\n{\n  int x = 0;\n}\n");
        assert!(found.iter().all(|pad| *pad == 0), "{found:?}");
    }

    #[test]
    fn the_columns_are_padded_to_the_widest_cell() {
        // Rows of `{"a", 1}` … `{"ccc", 3}`: the second column moves past the widest first cell (`"ccc"` = 5) and the
        // closing braces line up after the widest second cell (1 character, so one column wide).
        let found = padded("int t[] = {\n  {\"a\", 1},\n  {\"ccc\", 3}\n};\n");
        let non_zero: Vec<usize> = found.iter().copied().filter(|pad| *pad > 0).collect();
        assert!(!non_zero.is_empty(), "{found:?}");
        // Every row's `}` carries the same padding.
        let stream = crate::tokens::stream("int t[] = {\n  {\"a\", 1},\n  {\"ccc\", 3}\n};\n");
        let closes: Vec<usize> = rows(&stream).iter().map(|row| found[row.close]).collect();
        assert_eq!(closes.len(), 2);
        assert_eq!(closes[0], closes[1], "{closes:?}");
    }
}
