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
pub fn format(source: &str, stem: Option<&str>, style: &Style) -> String {
    // **P3 sorts includes before anything else**: it moves tokens, so it is a source transformation, not a
    // whitespace decision (see `includes.rs`). `stem` is the file's own name without its extension, which is how the
    // main header is recognised; with `SortIncludes.Enabled: false` the text is returned untouched.
    let source = &crate::includes::sorted(source, stem, style);
    let stream = tokens::stream(source);
    let roles = roles::of(source);
    let blocks = layout::blocks(&stream, &roles, style);
    // **Lines inside a braced initializer are continuations**, not block lines: `= {` at column 0 puts its elements
    // at `ContinuationIndentWidth`, which the block rules have no way to know.
    let init_depths = layout::init_depths(&stream, &roles);
    // **The rows of a braced table**, which `AlignArrayOfStructures: Left` pads into columns.
    let row_paddings = crate::rows::paddings(&stream, &roles, style);
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
    // **`AlignOperands: Align`**: the column of the first binary operator of the current statement, which is where a
    // continuation line that starts with one belongs (measured: `std::cerr << …` puts the first `<<` at 14 and the
    // next line's `<<` at 14 too).
    let mut operand_operator: Option<usize> = None;
    let indent_width = style.count(StyleOption::IndentWidth);
    // **A body opened on a line the bracket rules placed** (a lambda inside `f(...)`): the closer sits at that line
    // column and the body one indent in. Only bracket-placed lines count, so constructor lists and initializer rows
    // keep the level bookkeeping (§二十三 measured what happens when every continuation counts).
    let mut bracket_body: Option<(usize, usize)> = None;
    let mut brace_depth = 0usize;
    let mut line_was_bracket_placed = false;
    let mut line_column = 0usize;

    // **A `#` line is copied, not spaced.** Its internal spacing is the preprocessor's (`#  include` is legal,
    // `#include <a/b.h>` is the usual), and `docs/cpp-format.md` leaves `#define` bodies to P4 — a P0 that re-spaced
    // a directive would be a formatter that breaks macros. Measured before this guard: **143** gaps on the LuaParser
    // corpus where `#` and the directive name were pulled apart.
    // **P2: the breaks a column limit forces.** With `ColumnLimit: 0` — what the corpus asks for — nothing here
    // runs and the output is exactly what P0b/P1 produce. With a real limit, `breaks::break_line` says where a long
    // logical line has to break, and those breaks are inserted (existing source breaks are kept: this pass adds
    // breaks, it does not re-flow).
    let mut forced_breaks = vec![false; stream.tokens.len()];
    if style.count(StyleOption::ColumnLimit) > 0 {
        for line in crate::breaks::logical_lines(&stream) {
            let column = blocks.get(line.start).copied().flatten().unwrap_or(0);
            if let Some(breaks) = crate::breaks::break_line(&stream, &roles, style, &line, column) {
                for at in breaks {
                    forced_breaks[at] = true;
                }
            }
            // **And a source line that is itself too long gets its own search**: the whole-statement search cannot see
            // that this piece is already broken out and still overflows.
            for at in crate::breaks::break_over_long_pieces(&stream, &roles, style, &line, &blocks) {
                forced_breaks[at] = true;
            }
        }
    }

    // **`// clang-format off` … `on`**: the gaps inside those spans are the user`s (`regions.rs`).
    let disabled = crate::regions::per_token(&stream, &crate::regions::disabled_spans(source));

    let mut in_directive = false;

    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        let forced = forced_breaks[index];
        // **Inside a `// clang-format off` span the gap is the source's**, byte for byte: no re-spacing, no re-indenting.
        // The tokens are still seen (a brace in the span still opens a block), so the indentation after `on` is right.
        if disabled.get(index).copied().unwrap_or(false) {
            let gap_text = source.get(gap.start..gap.end).unwrap_or("");
            out.push_str(gap_text);
            column = match gap_text.rfind(char::from(10)) {
                Some(at) => columns.advance(0, &gap_text[at + 1..]),
                None => columns.advance(column, gap_text),
            };
        } else if forced && index > 0 && !in_directive {
            // **A break the column limit asked for**: the line continues at the continuation column.
            let indent = layout::continuation(&stack, statement_indent, stream.tokens[index].kind, style);
            out.push_str(line_ending);
            out.push_str(&" ".repeat(indent));
            column = indent;
        } else if gap.newlines == 0 && gap.comments.is_empty() && !gap.at_end && index > 0 && !in_directive {
            let spaces = space::between(&stream, index, &roles, style) + row_paddings[index];
            out.push_str(&" ".repeat(spaces));
            column += spaces;
        } else if !in_directive
            && (gap.comments.is_empty()
                // **A comment on its own line inside a braced initializer is an element of the table**: it lines up
                // with the elements around it (measured: `/* ORDER OPR */` belongs at the elements' column). A trailing
                // comment of the *previous* line may share the gap (`unsigned char right; /* … */` then the table's own
                // comment), which is why the last one is what counts, not the only one.
                || (gap
                    .comments
                    .last()
                    .is_some_and(|comment| comment.own_line)
                    && init_depths[index] > 0))
            && gap.newlines > 0
            && index > 0
            && let Some((indent, is_block_line)) = {
                // **A continuation line that starts with a binary operator aligns with the statement's first one**
                // (`AlignOperands: Align`), and that column beats the block rules.
                let aligned = operand_operator.filter(|_| {
                    style.text(StyleOption::AlignOperands) == "Align"
                        && layout::is_a_binary_operator(stream.tokens[index].kind)
                });
                // **A conditional's continuation (`?` or `:` at a line start) keeps the source's column.** clang-format
                // computes that column while breaking the line — in LuaLexer.cpp `? count + 2` sits at 19, which is
                // not the continuation indent — so the rules cannot derive it, and preserving it is what makes an
                // already-formatted file come back unchanged (the stability invariant I3).
                let conditional = matches!(
                    stream.tokens[index].kind,
                    cpp_parser::CppTokenKind::Question | cpp_parser::CppTokenKind::Colon
                )
                .then(|| source_indent(source, gap))
                .flatten();
                conditional
                    .map(|column| (column, false))
                    .or_else(|| aligned.map(|column| (column, false)))
                    .or_else(|| {
                        let inside = bracket_body.filter(|(depth, _)| *depth == brace_depth);
                        planned_indent(index, &stream, &blocks, &stack, statement_indent, style, in_ctor_init, ctor_column, init_depths[index] > 0)
                            .map(|(indent, is_block_line)| {
                                let closing = stream.tokens[index].kind == cpp_parser::CppTokenKind::RightBrace;
                                match inside {
                                    Some((_, column)) if is_block_line && closing => (column, true),
                                    Some((_, column)) if is_block_line => (column + indent_width, true),
                                    _ => (indent, is_block_line),
                                }
                            })
                    })
            }
        {
            // **A line that starts here**, with nothing in the gap but whitespace: the break count collapses to
            // `MaxEmptyLinesToKeep` and the column is the layout's. Everything else — a comment, a directive — is
            // still copied, which keeps this pass's blast radius to the lines it can decide.
            line_was_bracket_placed = !is_block_line && !stack.is_empty() && init_depths[index] == 0;
            line_column = indent;
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
            // **`EmptyLineBeforeAccessModifier`**: `Always` wants a blank line, `LogicalBlock` wants one when the
            // modifier starts a new group of declarations, `Never` wants none. Measured on the corpus: clang-format
            // writes a blank line before a `private:` that follows declarations.
            if layout::is_an_access_specifier(&stream, index) {
                let policy = style.text(StyleOption::EmptyLineBeforeAccessModifier);
                let after_declarations = matches!(
                    stream.tokens[index - 1].kind,
                    cpp_parser::CppTokenKind::Semicolon | cpp_parser::CppTokenKind::RightBrace
                );
                let at_block_start = stream.tokens[index - 1].kind == cpp_parser::CppTokenKind::LeftBrace;
                if !at_block_start
                    && (policy == "Always" || (policy == "LogicalBlock" && after_declarations))
                {
                    keep = keep.max(2);
                }
            }
            // **`EmptyLineAfterAccessModifier: Never`**: nothing blank between the modifier and the next line.
            if style.text(StyleOption::EmptyLineAfterAccessModifier) == "Never"
                && stream.tokens[index - 1].kind == cpp_parser::CppTokenKind::Colon
            {
                keep = 1;
            }
            // **A comment that starts a line replaces the break**: its own prefix carries the newline, so emitting the
            // breaks here as well was one blank line too many (measured on LuaDefine.h).
            let breaks_to_emit = if gap.comments.last().is_some_and(|comment| comment.own_line)
                && init_depths[index] > 0
            {
                0
            } else {
                keep
            };
            for _ in 0..breaks_to_emit {
                out.push_str(line_ending);
            }
            // **A trailing comment of the previous line is written first, from the source**: it belongs to that line,
            // and only the comment that starts this line is this pass's to place.
            let comment_in_this_gap = gap
                .comments
                .last()
                .filter(|comment| comment.own_line && init_depths[index] > 0);
            if let Some(comment) = comment_in_this_gap {
                // The prefix keeps its breaks but not its trailing indentation, which the computed column replaces:
                // keeping it stacked the source's two spaces onto the column (measured: 10 where 8 belongs).
                let prefix = source.get(gap.start..comment.start).unwrap_or("");
                out.push_str(prefix.trim_end_matches([' ', '\t']));
            }
            out.push_str(&" ".repeat(indent));
            column = indent;
            // **The comment that made this gap non-empty is a line of the block**: its text and everything after it
            // come from the source, written once, after the column this pass decided.
            if let Some(comment) = comment_in_this_gap {
                out.push_str(comment.text);
                // What follows the comment is a break to the token's own line: its newlines are kept (capped) and its
                // indentation is replaced by the column — the same one, because both lines are elements of the list.
                let tail = source.get(comment.end..gap.end).unwrap_or("");
                let breaks = tail
                    .chars()
                    .filter(|character| *character == '\n')
                    .count()
                    .min(max_empty + 1);
                for _ in 0..breaks {
                    out.push_str(line_ending);
                }
                if breaks > 0 {
                    out.push_str(&" ".repeat(indent));
                    column = indent;
                } else {
                    column = columns.advance(column, comment.text);
                }
            }
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

        // **The first binary operator of a statement records its column**, and the end of a statement forgets it.
        if operand_operator.is_none()
            && stream.gaps[index].newlines == 0
            && layout::is_a_binary_operator(stream.tokens[index].kind)
        {
            operand_operator = Some(column);
        }
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::Semicolon
                | cpp_parser::CppTokenKind::LeftBrace
                | cpp_parser::CppTokenKind::RightBrace
        ) {
            operand_operator = None;
        }

        // **The bracket a token closes leaves the stack before the token is written**, and one it opens is pushed
        // after — with its column filled in by the next token, if that token is on the same line.
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::RightParen | cpp_parser::CppTokenKind::RightBracket
        ) {
            stack.pop();
        }
        match stream.tokens[index].kind {
            cpp_parser::CppTokenKind::LeftBrace => {
                brace_depth += 1;
                if line_was_bracket_placed && !stack.is_empty() {
                    bracket_body = Some((brace_depth, line_column));
                }
                line_was_bracket_placed = false;
            }
            cpp_parser::CppTokenKind::RightBrace => {
                if let Some((depth, _)) = bracket_body && depth >= brace_depth {
                    bracket_body = None;
                }
                brace_depth = brace_depth.saturating_sub(1);
            }
            _ => {}
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
        assert_eq!(format(source, None, &llvm()), "int x = 1;\n");
    }

    #[test]
    fn comments_and_their_whitespace_are_left_alone() {
        let source = "int  x = 1;  //  keep   me\n";
        assert_eq!(format(source, None, &llvm()), "int x = 1;  //  keep   me\n");
    }

    #[test]
    fn a_constructor_initializer_list_and_the_class_body_after_it() {
        let source = "class T {\npublic:\n  T(const T &t)\n      : a(t.a),\n        b(t.b) {\n  }\n\nprivate:\n  int a;\n  int b;\n};\n";
        let formatted = format(source, None, &llvm());
        assert_eq!(formatted, source, "already formatted input must come back unchanged");
    }

    #[test]
    #[ignore = "an own-line comment inside a braced initializer keeps the source column: the line-start path is blocked by the comment, and the condition meant to allow it does not fire"]
    fn an_own_line_comment_inside_an_initializer_is_an_element() {
        let source = "static int table[] = {\n  /* note */\n  1,\n  2,\n};\n";
        let formatted = format(source, None, &llvm());
        assert_eq!(formatted, "static int table[] = {\n        /* note */\n        1,\n        2,\n};\n", "elements line up, the comment with them");
    }



    #[test]
    fn a_continuation_that_starts_with_an_operator_aligns_with_the_first_one() {
        // LLVM's `IndentWidth` is 2, so the body sits at 2 and the statement's first `<<` at 12; the continuation's
        // `<<` goes to the same column.
        let source = "void f()\n{\n  std::cerr << util::format(\"x\", y)\n            << std::endl;\n}\n";
        let formatted = format(source, None, &llvm());
        assert_eq!(
            formatted, source,
            "the `<<` of the continuation lines up with the statement's first `<<`"
        );
    }

    #[test]
    #[test]
    fn a_table_after_a_struct_body_indents_from_the_line_that_opened_it() {
        // The corpus`s widths and `ColumnLimit: 0`: the members sit at 4, the table`s comment and rows at 8
        // (measured against clang-format 22.1.3 on LuaDefine.h, which this shape is taken from).
        let mut style = llvm();
        style.set_from_text("IndentWidth", "4");
        style.set_from_text("ContinuationIndentWidth", "8");
        style.set_from_text("ColumnLimit", "0");
        let source = "static const struct\n{\n    unsigned char left;  /* left */\n    unsigned char right; /* right */\n} priority[] = {\n  /* ORDER OPR */\n        {10, 10},\n        {11, 11},\n};\n";
        let expected = "static const struct\n{\n    unsigned char left;  /* left */\n    unsigned char right; /* right */\n} priority[] = {\n        /* ORDER OPR */\n        {10, 10},\n        {11, 11},\n};\n";
        assert_eq!(format(source, None, &style), expected);
    }

    #[test]
    fn a_clang_format_off_span_is_copied_verbatim() {
        let source = "int  a = 1;\n// clang-format off\nint   b   =   2;\n// clang-format on\nint  c = 3;\n";
        let mut style = llvm();
        style.set_from_text("ColumnLimit", "0");
        let out = format(source, None, &style);
        assert!(out.contains("int   b   =   2;"), "the disabled span keeps its own spacing: {out:?}");
        assert!(out.contains("int a = 1;"), "the lines around it are formatted: {out:?}");
    }


    #[test]
    fn the_line_structure_is_kept_and_the_indentation_is_the_layout_s() {
        // **The line breaks stay the input's; the column is now decided** (P0b): `IndentWidth: 2` puts the body at 2,
        // and the spacing rules still apply inside the line.
        let source = "void f() {\n      int  x = 1;\n}\n";
        assert_eq!(format(source, None, &llvm()), "void f() {\n  int x = 1;\n}\n");
    }

    #[test]
    fn the_token_stream_is_preserved() {
        // The rule the module exists to keep: only whitespace changes.
        let source = "#include <vector>\n\ntemplate <class T>\nT  f(T  x) { return x; }\n";
        let formatted = format(source, None, &llvm());
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
        assert_eq!(format(source, None, &llvm()), "std::vector<int> v = {1, 2, 3};\n");
    }

    #[test]
    fn two_tokens_are_never_glued_into_one() {
        // `> >` must not become `>>`: the tree keeps the two `>` separate, and merging them would change the
        // reading of the file. The formatter declines the file instead of writing something else than it read.
        let source = "std::vector<std::vector<int> > v;\n";
        let formatted = format(source, None, &llvm());
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
/// The indentation the source gives the line this gap opens: the spaces after its last newline, if the gap holds
/// nothing but line breaks and that indentation.
fn source_indent(source: &str, gap: &tokens::Gap) -> Option<usize> {
    let text = source.get(gap.start..gap.end)?;
    let after_last_newline = text.rsplit(char::from(10)).next().unwrap_or(text);
    let only_spaces = after_last_newline.chars().all(|character| character == char::from(32) || character == char::from(9));
    if only_spaces && gap.newlines > 0 {
        Some(after_last_newline.chars().count())
    } else {
        None
    }
}

fn planned_indent(
    index: usize,
    stream: &tokens::Stream<'_>,
    blocks: &[Option<usize>],
    stack: &[usize],
    statement_indent: usize,
    style: &Style,
    in_ctor_init: bool,
    ctor_column: Option<usize>,
    in_initializer: bool,
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
    // **Inside a braced initializer the block rules do not apply**: the element is a continuation, indented by
    // `ContinuationIndentWidth` from the declaration (measured: clang-format writes 8 for a table declared at 0).
    if in_initializer && first != cpp_parser::CppTokenKind::RightBrace {
        // **The base is the line that opened the list**, not whatever block line came last: a struct body`s members
        // leave `statement_indent` at their level (4), while the table that follows belongs at 0 + the continuation
        // width (measured on LuaDefine.h: clang-format writes those rows and their comment at 8).
        let base = (0..index)
            .rev()
            .find(|earlier| !stream.gaps[*earlier].newlines.eq(&0) && blocks[*earlier].is_some())
            .and_then(|earlier| blocks[earlier])
            .unwrap_or(statement_indent);
        return Some((layout::continuation(stack, base, first, style), false));
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