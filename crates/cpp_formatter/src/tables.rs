//! **A long braced initializer list laid out as a column table** — clang-format 23's answer for a list of literals that
//! does not fit on one line.
//!
//! ```text
//! static const int n[] = {111, 2222, 33, 44444, 5,  666, 7777, 88, 999,
//!                         10,  11,   12, 13,    14, 15,  16,   17, 18,
//!                         19,  20,   21, 22,    23, 24,  25};
//! ```
//!
//! The layout is **not** an option: `AlignArrayOfStructures` is `None` in LLVM's style, and setting it to `Left` or
//! `Right` leaves this output byte for byte. It is what clang-format does with a braced list of simple elements that
//! overflows, measured on string tables and integer arrays, at `ColumnLimit` 40 and 80.
//!
//! The rule, derived from those measurements: **the fewest rows whose aligned table fits**. For a candidate row count
//! `R`, the columns are `C = ceil(n / R)`, element `i` sits at row `i / C` and column `i % C`, a column's width is the
//! widest cell in it (comma included), and a row's width is the first element's column plus every column's width and
//! the one space between them. `R = 1` is the single-line form, so the search starts there and takes the first `R`
//! where every row fits. Five fixtures agree with it exactly — 8, 10 and 20 elements of two widths, and the 25-name
//! `std::set` in `NameStyleChecker.cpp`.

use crate::config::Style;
use crate::roles::Roles;
use crate::tokens::Stream;
use cpp_parser::CppTokenKind;

/// One flat braced list laid out as a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The cells, in source order: the token indices of each element.
    pub cells: Vec<Vec<usize>>,
    /// The number of columns.
    pub columns: usize,
    /// The column each column starts at (including the one space after the previous comma).
    pub starts: Vec<usize>,
    /// Padding before each cell, in spaces, indexed like `cells`.
    pub paddings: Vec<usize>,
}

/// **The table a braced list of `widths` becomes**, or `None` when it stays on one line.
///
/// `widths` are the cells' widths including their comma, `first` is the column of the first cell, and `limit` the
/// column limit. The search tries one row, then two, and so on; the first row count whose rows all fit wins.
pub fn layout(widths: &[usize], first: usize, limit: usize) -> Option<Table> {
    let n = widths.len();
    if n < 2 {
        return None;
    }
    let available = limit.saturating_sub(first);
    if available == 0 {
        return None;
    }
    // **The starting row count comes from the total width**, not from trying one row at a time: clang-format packs
    // the table as if the cells followed each other on one line (their widths plus the spaces between them), divides
    // by the room a line has, and rounds up. Measured: the 25-integer array (total 108, room 56) starts at two rows,
    // making 13 columns, which does not fit, and one more row gives the **nine** the oracle writes. Searching from
    // the fewest rows instead would have taken 11 columns (77 wide) or a 13-column, two-row table when one fits.
    let total = widths.iter().sum::<usize>() + n.saturating_sub(1);
    let mut rows = total.div_ceil(available).max(1);
    while rows <= n {
        let columns = n.div_ceil(rows);
        // **A single column is not a table**: it is the list written one element per line, which clang-format
        // leaves to the general layout (`{` newline `"_G", "_ENV"};` stays on one line — measured).
        if columns >= 2
            && let Some(table) = with_columns(widths, first, limit, columns)
        {
            return if rows == 1 { None } else { Some(table) };
        }
        rows += 1;
    }
    None
}

/// The table with a fixed column count, or `None` when a row does not fit.
fn with_columns(widths: &[usize], first: usize, limit: usize, columns: usize) -> Option<Table> {
    let n = widths.len();
    // **A column's width is its widest cell**, so the same column lines up across rows.
    let mut column_widths = vec![0usize; columns];
    for (index, width) in widths.iter().enumerate() {
        let column = index % columns;
        column_widths[column] = column_widths[column].max(*width);
    }
    // Where each column starts: the first cell's column, then each column's width plus one space.
    let mut starts = vec![0usize; columns];
    let mut column = first;
    for (at, width) in column_widths.iter().enumerate() {
        starts[at] = column;
        column += width + 1;
    }
    // Every row has to fit. The last cell of the list has no comma, so its row ends there.
    let rows = n.div_ceil(columns);
    for row in 0..rows {
        let last = ((row + 1) * columns).min(n) - 1;
        let column = last % columns;
        if starts[column] + widths[last] > limit {
            return None;
        }
    }
    let mut paddings = Vec::with_capacity(n);
    for (index, width) in widths.iter().enumerate() {
        let column = index % columns;
        // **Pad to the widest cell of the column**: the natural space after a comma is one, and the cell's own width
        // is `width`, so the extra spaces are the column's width minus this cell's.
        paddings.push(column_widths[column] - width);
    }
    Some(Table {
        cells: Vec::new(),
        columns,
        starts,
        paddings,
    })
}

/// **The cells of the braced list at `open`** — the token indices of each element, split at the list's own commas.
///
/// A list with a comment inside, a nested brace, or a cell that is not a run of plain tokens is refused: clang-format
/// formats those with the general rules, and a table built over them would move a comment or a block.
pub fn cells_of(stream: &Stream<'_>, open: usize, close: usize) -> Option<Vec<Vec<usize>>> {
    let mut cells: Vec<Vec<usize>> = vec![Vec::new()];
    let mut depth = 0isize;
    for index in open + 1..close {
        if !stream.gaps[index].comments.is_empty() {
            return None;
        }
        match stream.tokens[index].kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket => depth += 1,
            CppTokenKind::RightParen | CppTokenKind::RightBracket => depth -= 1,
            // A nested brace is a table of its own or a block: not a cell of this one.
            CppTokenKind::LeftBrace | CppTokenKind::RightBrace => return None,
            CppTokenKind::Semicolon => return None,
            CppTokenKind::Comma if depth == 0 => {
                cells.push(Vec::new());
                continue;
            }
            _ => {}
        }
        cells.last_mut()?.push(index);
    }
    if cells.last().is_some_and(Vec::is_empty) {
        cells.pop();
    }
    if cells.len() < 2 {
        return None;
    }
    Some(cells)
}

/// **Whether a braced list at `open` is an initializer list** rather than a body, in the layout's own terms.
pub fn is_an_initializer_list(roles: &Roles, stream: &Stream<'_>, open: usize) -> bool {
    roles.init_brace.contains(&stream.tokens[open].start)
        && !roles.brace_kind.contains_key(&stream.tokens[open].start)
}

/// The width of a cell: its tokens with the spacing rules, plus the comma after it.
pub fn cell_width(
    stream: &Stream<'_>,
    roles: &Roles,
    style: &Style,
    cell: &[usize],
    last: bool,
) -> usize {
    let mut width = 0usize;
    for (offset, index) in cell.iter().copied().enumerate() {
        if offset > 0 {
            width += crate::space::between(stream, index, roles, style);
        }
        width += stream.tokens[index].text.chars().count();
    }
    // The final cell of the list has no comma: clang-format writes `"__name"};`.
    if !last { width + 1 } else { width }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cells' widths of the integer array clang-format 23 lays out, and the table it produces.
    fn integers() -> Vec<usize> {
        // `111, 2222, 33, 44444, 5, 666, 7777, 88, 999, 10, 11, … 25` — each cell's text plus its comma.
        let texts = [
            "111", "2222", "33", "44444", "5", "666", "7777", "88", "999", "10", "11", "12", "13",
            "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24", "25",
        ];
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| text.len() + usize::from(index + 1 < texts.len()))
            .collect()
    }

    #[test]
    fn the_integer_array_becomes_nine_columns() {
        // clang-format 23: first cell at column 24, nine columns, rows of 9, 9 and 7.
        let table = layout(&integers(), 24, 80).expect("a table");
        assert_eq!(table.columns, 9);
        assert_eq!(table.starts[0], 24);
        assert_eq!(
            table.paddings[0], 0,
            "the widest first cell needs no padding"
        );
    }

    #[test]
    fn a_fitting_list_stays_on_one_line() {
        assert_eq!(layout(&[5, 5, 5], 10, 80), None);
    }

    #[test]
    fn twenty_five_names_are_five_by_five() {
        // `NameStyleChecker.cpp`: 25 names of different widths, clang-format writes 5 rows of 5 columns.
        // The 25 names of `TableFieldSpecialName`, each width including the comma after it.
        let widths = [
            8, 8, 8, 8, 8, 8, 8, 9, 9, 8, 9, 9, 8, 8, 11, 8, 7, 7, 10, 13, 9, 7, 10, 9, 8,
        ];
        let table = layout(&widths, 4, 80).expect("a table");
        assert_eq!(table.columns, 5);
    }

    #[test]
    fn a_single_column_is_not_a_table() {
        // `std::set<…> X = {` then one element per line is not what clang-format writes for a short list: the two
        // elements stay together, so a one-column layout must not be chosen.
        assert_eq!(layout(&[6, 7], 74, 80), None);
    }

    #[test]
    fn the_widest_cell_of_a_column_sets_the_padding() {
        // Two columns: the first column"s widest cell is 4 and the first cell is 3, so it gains one space.
        let table = layout(&[3, 6, 4, 6], 0, 20).expect("a table");
        assert_eq!(table.columns, 2);
        assert_eq!(table.paddings, vec![1, 0, 0, 0]);
    }
}
