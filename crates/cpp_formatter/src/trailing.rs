//! **Where a trailing comment starts: `AlignTrailingComments` with `SpacesBeforeTrailingComments`.**
//!
//! Measured against clang-format 22.1.3 with `ColumnLimit: 0`, `IndentWidth: 4` and
//! `SpacesBeforeTrailingComments: N`:
//!
//! ```text
//!     A,   // one        a run of lines that each end in a comment is aligned:
//!     BB,  // two        every comment starts at  max(code end) + N
//!     CCCC,// three
//! int solo;// alone      a line with no neighbour on either side is  code end + N
//! ```
//!
//! The column is a property of the *group*, not of the line, which is why the earlier attempt (give every
//! lone comment `N` spaces and leave groups alone) measured 41 → 38: a group member is not a lone comment, and its
//! target is the group's column. This module computes that column per gap; `render` writes the spaces.

use crate::config::Style;
use crate::options::StyleOption;
use crate::tokens::Stream;

/// For every gap that carries a trailing comment, the column the comment must start at (`None` elsewhere).
pub fn columns(source: &str, stream: &Stream<'_>, style: &Style) -> Vec<Option<usize>> {
    let minimum = style.count(StyleOption::SpacesBeforeTrailingComments);
    // One entry per gap, not per token: a comment may be the last thing in the file, in the gap after the final token.
    let mut targets = vec![None; stream.gaps.len()];

    // The line each trailing comment sits on, and where its code ends.
    let mut found: Vec<(usize, usize, usize)> = Vec::new(); // (line, gap index, code end column)
    let mut line = 0usize;
    for index in 0..stream.gaps.len() {
        line += stream.gaps[index].newlines;
        let Some(comment) = stream.gaps[index].comments.last() else {
            continue;
        };
        if comment.own_line {
            continue;
        }
        // The code before the comment: from the start of the line up to the comment, without trailing spaces.
        let start_of_line = source[..comment.start].rfind('\n').map_or(0, |at| at + 1);
        let code = source[start_of_line..comment.start].trim_end();
        found.push((line, index, code.chars().count()));
    }

    // A group is a run of line numbers that touch: `line`, `line + 1`, … Each group shares one column.
    let mut start = 0usize;
    while start < found.len() {
        let mut end = start + 1;
        while end < found.len() && found[end].0 == found[end - 1].0 + 1 {
            end += 1;
        }
        let widest = found[start..end].iter().map(|(_, _, code_end)| *code_end).max().unwrap_or(0);
        for (_, index, _) in &found[start..end] {
            targets[*index] = Some(widest + minimum);
        }
        start = end;
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;
    use crate::tokens::stream;

    fn targets(source: &str, minimum: &str) -> Vec<Option<usize>> {
        let mut style = Style::default_style();
        style.set_from_text("SpacesBeforeTrailingComments", minimum);
        let stream = stream(source);
        columns(source, &stream, &style)
            .into_iter()
            .filter(|target| target.is_some())
            .collect()
    }

    #[test]
    fn a_run_of_comments_shares_one_column() {
        // `    A,` ends at 6, `    CCCC,` at 9: with the default minimum of one, both comments start at 10.
        let found = targets("enum E {\n    A, // one\n    CCCC,// two\n};\n", "1");
        assert_eq!(found, vec![Some(10), Some(10)]);
    }

    #[test]
    fn a_lone_comment_is_its_own_group() {
        // `int solo;` ends at 9; the next line has no comment, so the group is one line: 9 + 0.
        let found = targets("int solo;// alone\nint after;\n", "0");
        assert_eq!(found, vec![Some(9)]);
    }

    #[test]
    fn the_minimum_is_added_once() {
        let found = targets("int solo; // alone\n", "2");
        assert_eq!(found, vec![Some(11)]);
    }

    #[test]
    fn an_own_line_comment_is_not_a_trailing_one() {
        let found = targets("void f() {\n    // note\n}\n", "1");
        assert!(found.is_empty(), "{found:?}");
    }
}