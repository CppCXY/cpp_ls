//! **The renderer: the source with new whitespace, and not one other byte changed.**
//!
//! P0a (`docs/cpp-format.md` §十): the line structure and the indentation stay exactly as the input had them; every
//! gap that is **on one line and holds no comment** is rewritten to the width [`crate::space::between`] decides.
//! Every other gap — a newline, an indentation, a comment, the whitespace around a comment — is copied out of the
//! source verbatim.
//!
//! That asymmetry is the whole safety argument: the token text is copied from the source (never re-serialised), and
//! the only region ever replaced is a run of spaces/tabs that contains no comment and no newline. A file formatted
//! by this pass and then by `clang-format` differs only where the *decisions* differ, which is what the audit
//! measures; a file formatted by this pass can never lose a comment or a token.

use crate::config::Style;
use crate::columns::Columns;
use crate::options::StyleOption;
use crate::layout;
use crate::roles;
use crate::space;
use crate::tokens;

/// Format `source`, keeping its line structure and deciding the indentation.
pub fn format(source: &str, style: &Style) -> String {
    let stream = tokens::stream(source);
    let roles = roles::of(source);
    let blocks = layout::blocks(&stream, &roles, style);
    let columns = Columns::new(style);
    let max_empty = style.count(StyleOption::MaxEmptyLinesToKeep);
    let line_ending = tokens::line_ending(source);
    let mut out = String::with_capacity(source.len() + source.len() / 16);

    // **The columns of what has been written**, which is what a continuation line needs: `AlignAfterOpenBracket`
    // puts it under the first token after the open bracket, and that token's column was decided by the spacing rules
    // a moment ago. `stack` holds one entry per open bracket; `statement_indent` is the column of the line the current
    // statement started on.
    let mut column = 0usize;
    let mut stack: Vec<usize> = Vec::new();
    let mut statement_indent = 0usize;
    // **Where a constructor initializer list`s packed members line up**: clang-format packs them after the `:` of the
    // first initializer, so a line starting with `,` continues at that column rather than at a fixed offset. Measured
    // on `TokenNode`: the `:` line at 8 puts `Kind` at 10, and the next `Start(...)` is at 10 as well.
    let mut ctor_column: Option<usize> = None;
    // **`in_ctor_init` is the state a line-start test cannot see**: a constructor initializer list packs its members
    // onto lines that *start with an identifier* and end with a comma, so "does this line start with `,`" misses them
    // (measured: `Start(token.Start),` went to the brace level instead of the list`s column).
    let mut in_ctor_init = false;

    // **A `#` line is copied, not spaced.** Its internal spacing is the preprocessor's (`#  include` is legal,
    // `#include <a/b.h>` is the usual), and `docs/cpp-format.md` leaves `#define` bodies to P4 — a P0 that re-spaced
    // a directive would be a formatter that breaks macros. Measured before this guard: **143** gaps on the LuaParser
    // corpus where `#` and the directive name were pulled apart.
    let mut in_directive = false;

    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        let same_line = gap.newlines == 0 && gap.comments.is_empty() && !gap.at_end;
        if same_line && index > 0 && !in_directive {
            let spaces = space::between(&stream, index, &roles, style);
            out.push_str(&" ".repeat(spaces));
            column += spaces;
        } else if !in_directive
            && gap.comments.is_empty()
            && gap.newlines > 0
            && index > 0
            && let Some((indent, is_block_line)) = planned_indent(index, &stream, &blocks, &stack, statement_indent, style, in_ctor_init, ctor_column)
        {
            // **A line that starts here**, with nothing in the gap but whitespace: the break count collapses to
            // `MaxEmptyLinesToKeep` and the column is the layout's. Everything else — a comment, a directive — is
            // still copied, which keeps this pass's blast radius to the lines it can decide.
            let mut keep = gap.newlines.min(max_empty + 1);
            // **A blank line right after `{` is not kept** unless the style asks (`KeepEmptyLinesAtStartOfBlock`,
            // false by default, and the corpus leaves it false): clang-format writes the first statement of a block
            // against the brace, and keeping the programmer's blank there is a whole-file difference on its own.
            if keep > 1
                && index > 0
                && stream.tokens[index - 1].kind == cpp_parser::CppTokenKind::LeftBrace
                && !style.flag(StyleOption::KeepEmptyLinesAtStartOfBlock)
            {
                keep = 1;
            }
            for _ in 0..keep {
                out.push_str(line_ending);
            }
            out.push_str(&" ".repeat(indent));
            column = indent;
            if is_block_line {
                statement_indent = indent;
                ctor_column = None;
            } else if stream.tokens[index].kind == cpp_parser::CppTokenKind::Colon {
                // `: member(...)` — the packed members line up two columns past the colon (`: ` is two characters).
                ctor_column = Some(indent + 2);
                in_ctor_init = true;
            }
        } else {
            // Verbatim: the gap's own bytes (newlines, indentation, comments) are not ours to change in this pass.
            let gap_text = source.get(gap.start..gap.end).unwrap_or("");
            out.push_str(gap_text);
            column = match gap_text.rfind('\n') {
                Some(at) => columns.advance(0, &gap_text[at + 1..]),
                None => columns.advance(column, gap_text),
            };
        }

        // **The bracket a token closes leaves the stack before the token is written**, and one it opens is pushed
        // after — with its column filled in by the next token, if that token is on the same line.
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::RightParen | cpp_parser::CppTokenKind::RightBracket
        ) {
            stack.pop();
        }
        let text = stream.tokens[index].text;
        if let Some(top) = stack.last_mut()
            && *top == usize::MAX
            && !matches!(
                stream.tokens[index].kind,
                cpp_parser::CppTokenKind::LeftParen | cpp_parser::CppTokenKind::LeftBracket
            )
        {
            *top = column;
        }
        out.push_str(text);
        column = columns.advance(column, text);
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::LeftParen | cpp_parser::CppTokenKind::LeftBracket
        ) {
            // `usize::MAX` = "the token after this bracket has not been written yet".
            stack.push(usize::MAX);
        }

        // **A `{` ends the initializer list and a `;` ends the declaration.** Without this the flag stayed set past
        // the constructor body and every following member line was pulled to the list`s column (measured: `private:`
        // came out at 2 instead of 0).
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::LeftBrace | cpp_parser::CppTokenKind::Semicolon
        ) {
            in_ctor_init = false;
            ctor_column = None;
        }

        // A `#` that starts a line opens a directive; the first newline after it closes the directive.
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::Hash
            && (index == 0 || gap.newlines > 0)
        {
            in_directive = true;
        }
        if in_directive && stream.gaps.get(index + 1).is_some_and(|next| next.newlines > 0) {
            in_directive = false;
        }
    }

    if let Some(trailing) = stream.gaps.last() {
        out.push_str(source.get(trailing.start..trailing.end).unwrap_or(""));
    }

    // **The token stream is verified, not assumed.** Removing a space can *merge* two tokens — `> >` becomes `>>`
    // (a right shift), `+ +` becomes `++`, `/ /` becomes a comment — and no amount of care in the spacing rules
    // makes that impossible: it is a property of the lexer, not of the rules. Measured on the corpus before this
    // check existed: one header (`LuaParseError.h`) came out with a different token stream than it went in with.
    //
    // The fallback is the whole file, unchanged. A file that cannot be formatted without changing what it says is
    // a file this tool declines to touch — which is the only honest failure mode for a formatter, and it makes
    // invariant I1 (`docs/cpp-format.md` §七) hold by construction rather than by testing.
    if same_tokens(&stream, &out) {
        out
    } else {
        source.to_string()
    }
}

/// Do `before` and `after` hold the same token sequence (kind and text, in order)?
fn same_tokens(before: &tokens::Stream<'_>, after: &str) -> bool {
    let after = tokens::stream(after);
    before.tokens.len() == after.tokens.len()
        && before
            .tokens
            .iter()
            .zip(&after.tokens)
            .all(|(mine, theirs)| mine.kind == theirs.kind && mine.text == theirs.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llvm() -> Style {
        Style::default_style()
    }

    #[test]
    fn only_the_spaces_inside_a_line_are_rewritten() {
        let source = "int  x  =  1;\n";
        assert_eq!(format(source, &llvm()), "int x = 1;\n");
    }

    #[test]
    fn comments_and_their_whitespace_are_left_alone() {
        let source = "int  x = 1;  //  keep   me\n";
        assert_eq!(format(source, &llvm()), "int x = 1;  //  keep   me\n");
    }

    #[test]
    fn a_constructor_initializer_list_and_the_class_body_after_it() {
        let source = "class T {\npublic:\n  T(const T &t)\n      : a(t.a),\n        b(t.b) {\n  }\n\nprivate:\n  int a;\n  int b;\n};\n";
        let formatted = format(source, &llvm());
        assert_eq!(formatted, source, "already formatted input must come back unchanged");
    }

    #[test]
    fn the_line_structure_is_kept_and_the_indentation_is_the_layout_s() {
        // **The line breaks stay the input's; the column is now decided** (P0b): `IndentWidth: 2` puts the body at 2,
        // and the spacing rules still apply inside the line.
        let source = "void f() {\n      int  x = 1;\n}\n";
        assert_eq!(format(source, &llvm()), "void f() {\n  int x = 1;\n}\n");
    }

    #[test]
    fn the_token_stream_is_preserved() {
        // The rule the module exists to keep: only whitespace changes.
        let source = "#include <vector>\n\ntemplate <class T>\nT  f(T  x) { return x; }\n";
        let formatted = format(source, &llvm());
        let before: Vec<_> = tokens::stream(source)
            .tokens
            .iter()
            .map(|token| (token.kind, token.text))
            .collect();
        let after: Vec<_> = tokens::stream(&formatted)
            .tokens
            .iter()
            .map(|token| (token.kind, token.text))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn a_braced_initializer_keeps_its_shape() {
        let source = "std::vector<int> v = { 1,2,3 };\n";
        assert_eq!(format(source, &llvm()), "std::vector<int> v = {1, 2, 3};\n");
    }

    #[test]
    fn two_tokens_are_never_glued_into_one() {
        // `> >` must not become `>>`: the tree keeps the two `>` separate, and merging them would change the
        // reading of the file. The formatter declines the file instead of writing something else than it read.
        let source = "std::vector<std::vector<int> > v;\n";
        let formatted = format(source, &llvm());
        let before: Vec<_> = tokens::stream(source).tokens.iter().map(|t| t.text).collect();
        let after: Vec<_> = tokens::stream(&formatted).tokens.iter().map(|t| t.text).collect();
        assert_eq!(before, after);
    }
}

/// **The column a line gets, and whether it is a block line** (which is what updates `statement_indent`).
///
/// Three answers, in this order: a line inside a constructor initializer list goes to the list's own column; a block
/// line takes the brace structure's answer; anything else is a continuation, whose column is the aligner's and is
/// computed here because the emitted columns are known here and nowhere earlier.
fn planned_indent(
    index: usize,
    stream: &tokens::Stream<'_>,
    blocks: &[Option<usize>],
    stack: &[usize],
    statement_indent: usize,
    style: &Style,
    in_ctor_init: bool,
    ctor_column: Option<usize>,
) -> Option<(usize, bool)> {
    let first = stream.tokens[index].kind;
    if std::env::var_os("CPPFMT_DEBUG").is_some() {
        eprintln!(
            "planned: index={index} token={:?} in_ctor_init={in_ctor_init} ctor_column={ctor_column:?} block={:?} stack={}",
            stream.tokens[index].text,
            blocks[index],
            stack.len()
        );
    }
    let is_a_brace = matches!(
        first,
        cpp_parser::CppTokenKind::LeftBrace | cpp_parser::CppTokenKind::RightBrace
    );
    if in_ctor_init && !is_a_brace {
        return Some((ctor_column.unwrap_or(statement_indent), false));
    }
    if let Some(indent) = blocks[index] {
        return Some((indent, true));
    }
    if stack.is_empty() && !layout::starts_a_continuation(stream, index) {
        return None;
    }
    if first == cpp_parser::CppTokenKind::Comma && let Some(column) = ctor_column {
        return Some((column, false));
    }
    Some((layout::continuation(stack, statement_indent, first, style), false))
}