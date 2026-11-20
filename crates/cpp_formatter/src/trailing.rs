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
    let mut found: Vec<(usize, usize, usize, usize)> = Vec::new(); // (line, gap, code end column, comment width)
    let mut line = 0usize;
    for (index, gap) in stream.gaps.iter().enumerate() {
        line += gap.newlines;
        let Some(comment) = gap.comments.last() else {
            continue;
        };
        if comment.own_line {
            continue;
        }
        // The code before the comment: from the start of the line up to the comment, without trailing spaces.
        let start_of_line = source[..comment.start].rfind('\n').map_or(0, |at| at + 1);
        let code = source[start_of_line..comment.start].trim_end();
        found.push((
            line,
            index,
            code.chars().count(),
            comment.text.trim_end().chars().count(),
        ));
    }

    // A group is a run of line numbers that touch: `line`, `line + 1`, … Each group shares one column.
    let mut start = 0usize;
    while start < found.len() {
        let mut end = start + 1;
        while end < found.len() && found[end].0 == found[end - 1].0 + 1 {
            end += 1;
        }
        // **A comment that cannot be aligned keeps its own start and splits the run** (`run_columns`).
        let entries: Vec<(usize, usize, usize)> = found[start..end]
            .iter()
            .map(|(_, _, code_end, comment_width)| (*code_end, *comment_width, minimum))
            .collect();
        let limit = style.count(StyleOption::ColumnLimit);
        let columns = run_columns(&entries, limit);
        for (offset, (_, index, code_end, _)) in found[start..end].iter().enumerate() {
            targets[*index] = Some(match columns[offset] {
                Some(column) => column,
                None => code_end + minimum,
            });
        }
        start = end;
    }

    targets
}

/// **The second scan: align the trailing comments of the rendered output.**
///
/// The first scan writes the code; this one reads the result back and only touches the spaces in front of a trailing
/// comment. That order matters: the first attempt computed target columns from the *source* and regressed 42 → 38,
/// because the formatter's own columns differ from the source's on the lines it re-indents. Here the code end is read
/// off the line that was actually written, so the two can never disagree.
///
/// A *trailing* comment is one with code before it on the same line. A run of consecutive lines that each have one is
/// a group (the measure in this module's header), and every comment in a group starts at `max(code end) + N`.
pub fn align(output: &str, style: &Style) -> String {
    // **clang-format's rule, read from `TokenAnnotator.cpp:4208` and `WhitespaceManager::alignTrailingComments`**:
    //
    // ```text
    // each trailing comment starts at  code end + SpacesBeforeTrailingComments, lifted by one when the comment is a
    // block comment (`spaceRequiredBefore`: `;//` is fine, `;/*` is not);
    // a run of consecutive lines is one sequence; the column is the maximum of those starts;
    // a comment only ever moves right (`if (Shift <= 0) continue`).
    // ```
    //
    // That one asymmetry — a line comment sits against a comma, a block comment does not — is what the earlier
    // attempts missed: `TK_GLOBAL,// …` really is at the code's end, and `unsigned char left;  /* … */` really is two
    // columns further out because its neighbour `right;` pushes the whole sequence there.
    let minimum = style.count(StyleOption::SpacesBeforeTrailingComments);
    let lines: Vec<&str> = output.split_inclusive('\n').collect();
    if lines.is_empty() {
        return output.to_string();
    }

    let mut found: Vec<(usize, &str, &str)> = Vec::new();
    // **The scan carries the block-comment state across lines**: a `//` inside `/* … */` is comment text, not a
    // trailing comment (see `split_trailing_comment`).
    let mut in_block = false;
    for (index, line) in lines.iter().enumerate() {
        if let Some((code, comment)) = split_trailing_comment(line, &mut in_block) {
            found.push((index, code.trim_end(), comment));
        }
    }

    let mut rewritten: Vec<Option<String>> = vec![None; lines.len()];
    // The lines that hold nothing but a comment: a run of trailing comments **continues through them**, because
    // clang-format aligns across a doc comment's continuation line (measured: `a_long_name, ///< first` then
    // `             ///< second` then `b, ///< bee` — the `b` and `cc` comments align to `a_long_name,`'s, not to each
    // other). A line with code, and a blank line, do break the run.
    let comment_only: Vec<bool> = lines
        .iter()
        .map(|line| line.trim_start().starts_with("//") || line.trim_start().starts_with("/*"))
        .collect();
    let continues_through_comments =
        |from: usize, to: usize| from < to && (from + 1..to).all(|between| comment_only[between]);
    let mut trailing_column: Vec<Option<usize>> = vec![None; lines.len()];
    let mut start = 0usize;
    while start < found.len() {
        let mut end = start + 1;
        while end < found.len()
            && (found[end].0 == found[end - 1].0 + 1
                || continues_through_comments(found[end - 1].0, found[end].0))
        {
            end += 1;
        }
        // **The column of the sequence, or the comment's own start when it cannot be aligned** (`run_columns`):
        // measured on nlohmann json's include block, where a 67-column comment would push every shorter line two
        // columns right of what clang-format writes.
        let limit = style.count(StyleOption::ColumnLimit);
        let entries: Vec<(usize, usize, usize)> = found[start..end]
            .iter()
            .map(|(_, code, comment)| {
                (
                    width(code),
                    width(comment.trim_end()),
                    start_spaces(code, comment, minimum),
                )
            })
            .collect();
        let columns = run_columns(&entries, limit);
        for (offset, (index, code, comment)) in found[start..end].iter().enumerate() {
            let spaces = match columns[offset] {
                Some(column) => {
                    trailing_column[*index] = Some(column);
                    column.saturating_sub(width(code))
                }
                None => start_spaces(code, comment, minimum),
            };
            rewritten[*index] = Some(format!("{code}{}{comment}", " ".repeat(spaces)));
        }
        start = end;
    }

    // **A line that holds only a line comment directly under a trailing one starts at that comment's column**: that is
    // what clang-format does for the continuation line of a doc comment (`///< actual value` under `///< …`), measured
    // on nlohmann json's `lexer.hpp` and on a two-line `// one` / `// two` fixture.
    let indentation = |line: &str| {
        line.chars()
            .take_while(|character| *character == ' ')
            .count()
    };
    for index in 1..lines.len() {
        if !comment_only[index] {
            continue;
        }
        let Some(column) = trailing_column[index - 1] else {
            continue;
        };
        let trimmed = lines[index].trim_start();
        // **A doc comment`s continuation is pulled in, a plain `//` line is not**: the two are told apart by the
        // marker (`///`, `//!`) — measured on nlohmann json`s `json_sax.hpp`, where `// Handle all possible types …`
        // sits at the case body`s own column and the first version of this rule pulled it to the trailing comment`s.
        if !trimmed.starts_with("///") {
            continue;
        }
        // **Only a comment written as a continuation is pulled in**: one that sits at the code's own indentation is a
        // comment block of its own (measured: `int aaaa; // one` then `// two` at column 0 stays at column 0, while
        // `// two` written five columns in moves to the trailing comment's column).
        if indentation(lines[index]) <= indentation(lines[index - 1]) {
            continue;
        }
        let ending = if trimmed.ends_with('\n') { "\n" } else { "" };
        rewritten[index] = Some(format!(
            "{}{}{ending}",
            " ".repeat(column),
            trimmed.trim_end()
        ));
    }

    let mut out = String::with_capacity(output.len());
    for (index, line) in lines.iter().enumerate() {
        match &rewritten[index] {
            Some(replacement) => out.push_str(replacement),
            None => out.push_str(line),
        }
    }
    out
}

/// **The column each trailing comment of one run starts at** — `None` when the comment cannot be aligned and keeps its
/// own start.
///
/// `entries` are `(code width, comment width, the start the comment asks for on its own)`. A run is one sequence of
/// consecutive lines that each end in a comment; its column is the furthest start any of them asks for — **unless that
/// would push a comment past the column limit**. In that case the offending comment keeps its own start and the run is
/// **split there**: measured on nlohmann json, where
///
/// ```text
/// #include <algorithm>  // any_of
/// #include <type_traits> // conditional, integral_constant, is_signed, true_type, false_type
/// #include <utility>    // move
/// ```
///
/// aligns the first two comments to the same column, gives `<type_traits>` its own one space (a column of 23 would push
/// its 67-column comment past 80) and starts a new run after it, so `<utility>` comes back with a single space too.
fn run_columns(entries: &[(usize, usize, usize)], limit: usize) -> Vec<Option<usize>> {
    let mut columns = vec![None; entries.len()];
    let mut pending: Vec<(usize, usize)> = vec![(0, entries.len())];
    while let Some((start, end)) = pending.pop() {
        if start >= end {
            continue;
        }
        let column = (start..end)
            .map(|at| entries[at].0 + entries[at].2)
            .max()
            .unwrap_or(0);
        // **A comment that cannot fit even on a line of its own does not split the run**: splitting would not help
        // it, and clang-format keeps the earlier lines aligned (`binary_reader.hpp`'s 100-column `// false_type,
        // integral_constant, …` stays in the include block's column 23). A comment that *would* fit on its own line
        // but not at the run's column is the offender the run splits at.
        let too_long = (start..end)
            .find(|at| limit > 0 && column + entries[*at].1 > limit && entries[*at].1 <= limit);
        match too_long {
            Some(at) => {
                // The offending comment keeps its own start (`None`), and the run continues **after** it.
                pending.push((at + 1, end));
                pending.push((start, at));
            }
            None => {
                columns[start..end].fill(Some(column));
            }
        }
    }
    columns
}
/// The spaces a comment asks for before alignment: `SpacesBeforeTrailingComments`, and one more for a block comment
/// (`spaceRequiredBefore` in clang-format: `;//` needs no space, `;/*` does).
fn start_spaces(code: &str, comment: &str, minimum: usize) -> usize {
    // **A cell of a braced array initializer** is laid out by `WhitespaceManager::getCells`, which puts a trailing
    // comment "a space past the last token", and two when that token is not a comma:
    // `setChangeSpaces(i, Changes[i - 1].Tok->is(tok::comma) ? 1 : 2)`. That is why a row ending in `,` has one
    // space before its comment and a row ending in `}` has two.
    if code.trim_start().starts_with('{') && code.trim_end().ends_with('}') {
        return minimum + 2;
    }
    if code.trim_start().starts_with('{') && code.trim_end().ends_with(',') {
        return minimum + 1;
    }
    // **A block comment asks for *at least* one space, not one more than the minimum**: with
    // `SpacesBeforeTrailingComments: 1` clang-format writes `a = 1; /* inline */` on a lone line, and with 0 it
    // writes the same one — so the rule is `max(minimum, 1)`, not `minimum + 1` (measured on the snippet
    // corpus, where `minimum + 1` produced two spaces).
    if comment.starts_with("/*") {
        minimum.max(1)
    } else {
        minimum
    }
}
// Split one line into (code, comment) when it has a trailing comment; `None` for a line that has none.

///
/// The scan skips string and character literals so that `const char *s = "// not a comment";` stays code, and a line
/// whose first non-space characters are the comment is not a trailing one (it is an own-line comment).
fn split_trailing_comment<'a>(line: &'a str, in_block: &mut bool) -> Option<(&'a str, &'a str)> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut quote: Option<u8> = None;
    let mut seen_code = false;
    while index < bytes.len() {
        let byte = bytes[index];
        // **A line that starts inside a block comment is comment text**, not code: `http://x` inside `/* … */` is not
        // a trailing comment, and splitting it inserted a space after the scheme (`http: //x`, measured on
        // nlohmann json's `to_chars.hpp`, whose doc comment carries a URL).
        if *in_block {
            if byte == b'*' && index + 1 < bytes.len() && bytes[index + 1] == b'/' {
                *in_block = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(open) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                seen_code = true;
                index += 1;
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                // A block comment that opens here. When it closes on this line and only whitespace follows, it **is**
                // the trailing comment; when it does not close, the line ends inside it and the next line starts in
                // the block (`*in_block`), where a `//` is text rather than a comment.
                match line[index + 2..].find("*/") {
                    Some(close) => {
                        let after = index + 2 + close + 2;
                        if line[after..].trim().is_empty() {
                            if !seen_code {
                                return None;
                            }
                            return Some((&line[..index], &line[index..]));
                        }
                        index = after;
                        seen_code = true;
                    }
                    None => {
                        *in_block = true;
                        return None;
                    }
                }
            }
            b'/' if index + 1 < bytes.len()
                && (bytes[index + 1] == b'/' || bytes[index + 1] == b'*') =>
            {
                if !seen_code {
                    return None;
                }
                // **A comment is a *trailing* one only when it ends the line**: `return /* c */ 1;` has code after it,
                // and treating that as a trailing comment moved it (measured: our output gained a space before every
                // inline block comment). A line comment always runs to the end; a block comment must close with only
                // whitespace after it.
                let rest = &line[index..];
                let ends_the_line = if bytes[index + 1] == b'/' {
                    true
                } else {
                    rest.find("*/")
                        .is_some_and(|close| rest[close + 2..].trim().is_empty())
                };
                if !ends_the_line {
                    return None;
                }
                return Some((&line[..index], rest));
            }
            b' ' | b'\t' | b'\r' | b'\n' => index += 1,
            _ => {
                seen_code = true;
                index += 1;
            }
        }
    }
    None
}

/// The display width of a piece of code (tabs to the next multiple of eight, wide characters two columns).
fn width(text: &str) -> usize {
    crate::columns::Columns::new(&Style::default_style()).advance(0, text)
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
    fn a_comment_past_the_column_limit_keeps_its_own_start_and_splits_the_run() {
        // clang-format 23.1.5, `LLVM` (ColumnLimit 80): the includes below align to 21, `<type_traits>` (22 columns
        // of code plus a 67-column comment) keeps one space, and `<utility>` starts a new run.
        let source = "\
#include <algorithm> // any_of
#include <array> // array
#include <type_traits> // conditional, integral_constant, is_signed, true_type, false_type
#include <utility> // move
";
        let style = Style::default_style();
        let expected = "\
#include <algorithm> // any_of
#include <array>     // array
#include <type_traits> // conditional, integral_constant, is_signed, true_type, false_type
#include <utility> // move
";
        assert_eq!(align(source, &style), expected);
    }

    #[test]
    fn a_url_inside_a_block_comment_is_not_a_trailing_comment() {
        // clang-format 23.1.5 leaves a doc comment's URL alone; splitting it inserted a space after the scheme
        // (`http: //x`, measured on nlohmann json's `to_chars.hpp`).
        let source = "/*!\n@brief x\nhttp://example.com/a (b).\n*/\nint x;\n";
        let style = Style::default_style();
        assert_eq!(align(source, &style), source);
    }

    #[test]
    fn the_minimum_is_added_once() {
        let found = targets("int solo; // alone\n", "2");
        assert_eq!(found, vec![Some(11)]);
    }

    #[test]
    fn a_run_is_aligned_and_a_lone_comment_is_not() {
        let output = "enum E {\n    A, // one\n    CCCC,// two\n};\nint solo; // alone\n";
        let mut style = Style::default_style();
        style.set_from_text("SpacesBeforeTrailingComments", "1");
        // `    CCCC,` ends at 9, so both comments in the run start at 10; `int solo;` ends at 9 and is alone, so
        // its comment starts at 10 as well — the source already had that.
        assert_eq!(
            align(output, &style),
            "enum E {\n    A,    // one\n    CCCC, // two\n};\nint solo; // alone\n"
        );
    }

    #[test]
    fn a_comment_inside_a_string_is_not_a_comment() {
        let mut style = Style::default_style();
        style.set_from_text("SpacesBeforeTrailingComments", "0");
        let output = "const char *s = \"// not a comment\";\n";
        assert_eq!(align(output, &style), output);
    }

    #[test]
    fn a_line_comment_sits_against_the_code_and_a_block_comment_does_not() {
        // The two cases the corpus needed: `TK_GLOBAL,// …` (a line comment, so no space) and the two struct members
        // (`/* … */`, so one space, and the sequence pushes both to the wider line`s column).
        let mut style = Style::default_style();
        style.set_from_text("SpacesBeforeTrailingComments", "0");
        assert_eq!(
            align("    TK_GLOBAL, // x\n", &style),
            "    TK_GLOBAL,// x\n"
        );
        assert_eq!(
            align(
                "    unsigned char left; /* a */\n    unsigned char right; /* b */\n",
                &style
            ),
            "    unsigned char left;  /* a */\n    unsigned char right; /* b */\n"
        );
    }

    #[test]
    fn an_own_line_comment_is_not_a_trailing_one() {
        let found = targets("void f() {\n    // note\n}\n", "1");
        assert!(found.is_empty(), "{found:?}");
    }
}
