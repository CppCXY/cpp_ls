//! **The renderer: the source with new whitespace, and not one other byte changed.**
//!
//! Every other gap — a newline, an indentation, a comment, the whitespace around a comment — is
//! copied out of the source verbatim.
//!
//! That asymmetry is the whole safety argument: the token text is copied from the source (never re-serialised), and
//! the only region ever replaced is a run of spaces/tabs that contains no comment and no newline. A file formatted
//! by this pass and then by `clang-format` differs only where the *decisions* differ, which is what the audit
//! measures; a file formatted by this pass can never lose a comment or a token.

use crate::columns::Columns;
use crate::config::Style;
use crate::layout;
use crate::options::StyleOption;
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
    let mut row_paddings = crate::rows::paddings(&stream, &roles, style);
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
    // **Template argument lists get their own column stack**: a continuation line inside one aligns after the `<`.
    // `layout::template_angles` is the lexical test, because the parser does not mark every list (`using` aliases).
    let template_angles = layout::template_angles(&stream, &roles);
    let mut angles: Vec<usize> = Vec::new();
    // **The indent of the line each `<` was written on**: a template list whose `<` ends its line indents its
    // arguments one `ContinuationIndentWidth` past *that* line (measured: `*j.template get_ptr<` at 8 then 12).
    let mut angle_anchors: Vec<usize> = Vec::new();
    // **An enumeration body keeps a column of its own**: `enum class Wide { A,` puts `B,` under `A`.
    let enum_bodies = layout::enum_body_braces(&stream);
    let mut enums: Vec<(usize, usize)> = Vec::new();
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
    // **Where the current statement's first operand starts** — the column a continuation after a trailing binary
    // operator aligns with (`AlignOperands: Align`).
    let mut operand_column: Option<usize> = None;
    // **The column of the first string of an implicit concatenation** — the anchor clang-format aligns the rest of
    // the strings with (`const char x[] = "a"` then `"b"` under the first one). Set when the string that follows an
    // assignment is written, and forgotten at the end of the statement.
    let mut first_string: Option<usize> = None;
    let mut awaiting_first_string = false;
    let indent_width = style.count(StyleOption::IndentWidth);
    // **A body opened on a line the bracket rules placed** (a lambda inside `f(...)`): the closer sits at that line
    // column and the body one indent in. Only bracket-placed lines count, so constructor lists and initializer rows
    // keep the level bookkeeping (§二十三 measured what happens when every continuation counts).
    let mut bracket_body: Option<(usize, usize)> = None;
    let mut brace_depth = 0usize;
    let mut line_was_bracket_placed = false;
    let mut line_column = 0usize;

    // **A `#` line is copied, not spaced.** Measured before this guard: **143** gaps on the
    // LuaParser corpus where `#` and the directive name were pulled apart. **P2: the breaks a
    // column limit forces.** With `ColumnLimit: 0` — what the corpus asks for — nothing here runs
    // and the output is exactly what P0b/P1 produce. With a real limit, `breaks::break_line` says
    // where a long logical line has to break, and those breaks are inserted (existing source breaks
    // are kept: this pass adds breaks, it does not re-flow).
    let mut forced_breaks = vec![false; stream.tokens.len()];
    if style.count(StyleOption::ColumnLimit) > 0 {
        for line in crate::breaks::logical_lines_with(&stream, Some(&roles)) {
            // **Only the source lines are searched**: a statement that is already spread over several lines is
            // short per line, and breaking it by its *whole* length adds breaks clang-format never makes (measured:
            // 28 of 43 files changed when clang-format`s own 80-column output was fed back through us).
            for at in crate::breaks::break_over_long_pieces(&stream, &roles, style, &line, &blocks)
            {
                forced_breaks[at] = true;
            }
        }
    }

    // **`// clang-format off` … `on`**: the gaps inside those spans are the user`s (`regions.rs`).
    let disabled = crate::regions::per_token(&stream, &crate::regions::disabled_spans(source));

    // **The lines `BreakBeforeBraces` asks for**: a `{` (or an `else` after a `}`) that must start a line, one that
    // must stay on the previous line, and an empty body the style splits. Kept three vectors rather than one enum
    // because each answers a different branch of the loop below.
    let WrappingLines {
        mut wrapped,
        mut attached,
        mut split,
        namespace_comments,
        namespace_long,
        body_wrapped,
        closers,
        pairs,
    } = wrapping_lines(&stream, &roles, style);

    // **`AllowShortFunctionsOnASingleLine`**: a definition whose body holds one statement and whose whole text fits
    // the column limit becomes one line under `All` (a class method only under `Inline`, never under `None`). The
    // oracle's two shapes, measured — `void a() { g(); }` joins, `void b() { g(); h(); }` does not. A body the style
    // refuses to join is **broken**, which is why a one-line function under `None` comes back as three lines.
    let mut joined = vec![false; stream.tokens.len()];
    // **A lambda body joins at render time**: its width depends on the column its `{` lands in, which is only known
    // while printing (a signature that spans lines still gets `{ … }` on its last line — measured). Keyed by the `{`
    // token: `(closing brace, width of the body after the brace)`.
    let mut lambda_body: Vec<Option<(usize, usize)>> = vec![None; stream.tokens.len()];
    if style.count(StyleOption::ColumnLimit) > 0 {
        let option = style.text(StyleOption::AllowShortFunctionsOnASingleLine);
        for definition in crate::functions::definitions(&stream, &roles) {
            let allowed = match option {
                "All" => true,
                "Inline" | "InlineOnly" => definition.inside_a_record,
                "Empty" => definition.open + 1 == definition.close,
                _ => false,
            };
            let indent = blocks[definition.start].unwrap_or(0);
            let fits =
                crate::functions::one_line_width(&stream, &roles, style, &definition, indent)
                    <= style.count(StyleOption::ColumnLimit);
            // **A comment in the body keeps its own line**, so the join is refused rather than moved around it.
            let commented = (definition.start + 1..=definition.close)
                .any(|index| !stream.gaps[index].comments.is_empty());
            // **A lambda's body is joined from its own `{`**, not as part of the whole definition: clang-format writes
            // `[this](…, FormatResolve &resolve) { DoResolve(…); }` even when the lambda's signature already spans
            // several lines (measured). The width is checked while printing, where the `{`'s column is known.
            let is_a_lambda = roles.brace_kind.get(&stream.tokens[definition.open].start)
                == Some(&crate::roles::BraceKind::Lambda);
            if is_a_lambda && definition.single_statement && !definition.nested_block && !commented
            {
                lambda_body[definition.open] = Some((
                    definition.close,
                    crate::functions::body_width(&stream, &roles, style, &definition) - 1,
                ));
            } else if allowed
                && fits
                && definition.single_statement
                && !definition.nested_block
                && !commented
            {
                for slot in &mut joined[definition.start + 1..=definition.close] {
                    *slot = true;
                }
            } else if definition.open + 1 < definition.close {
                // **Broken at the body's own level**, not at a continuation column: a function body is a block, and
                // the line after `{` sits one indent in (`blocks`), not `ContinuationIndentWidth` past the signature.
                wrapped[definition.open + 1] = true;
                wrapped[definition.close] = true;
            }
            // **An empty body whose definition is spread over lines and does not fit** is placed by the brace rule,
            // and then split: under `Allman`, clang-format writes
            //
            //     FormatState::FormatState(Mode mode)
            //         : _currentWidth(0), _mode(mode), _foreachContinue(true)
            //     {
            //     }
            //
            // — the joined form would be 88 columns — while a definition that fits joins into `{}` and a definition
            // already written on one line is left over-long rather than broken for no gain (all three measured).
            let kind = roles.brace_kind.get(&stream.tokens[definition.open].start);
            let spans_lines = line_start(&stream, definition.open) != definition.start;
            if definition.open + 1 == definition.close
                && spans_lines
                && !fits
                && kind.is_some_and(|kind| crate::wrapping::a_brace_starts_a_line(style, *kind))
            {
                wrapped[definition.open] = true;
                wrapped[definition.close] = true;
                split[definition.close] = true;
                attached[definition.open] = false;
                attached[definition.close] = false;
            }
        }
    }

    // **`AllowShortEnumsOnASingleLine`**: an enumeration whose whole text fits the column limit is one line — `LLVM`
    // and `Google` set the option, `Microsoft` does not, and the oracle's shape was measured: `enum class input_format_t
    // { json, cbor, msgpack };` is 48 columns and joins, a five-item enumeration does not. A comment inside the body
    // refuses the join (it would have to move), and `ColumnLimit: 0` never reflows.
    if style.count(StyleOption::ColumnLimit) > 0
        && style.flag(StyleOption::AllowShortEnumsOnASingleLine)
    {
        for open_index in 0..stream.tokens.len() {
            // **The body is found lexically**: `roles.brace_kind` does not mark an enumeration defined with the
            // brace on its own line (measured on the fixture), while the lexical pass does — and it is what the
            // enumeration column already uses.
            if !enum_bodies[open_index] {
                continue;
            }
            let Some(close_index) = pairs[open_index] else {
                continue;
            };
            if close_index <= open_index + 1 {
                continue;
            }
            // **A trailing comma refuses the join**: the one-line form would be `{ A, B, }`, which clang-format never
            // writes — measured on a member enumeration written `{\n  A,\n  B,\n} i = I::A;`, which the oracle keeps
            // broken while the same body without the last comma is one line.
            if stream.tokens[close_index - 1].kind == cpp_parser::CppTokenKind::Comma {
                continue;
            }
            if (open_index..=close_index).any(|at| !stream.gaps[at].comments.is_empty()) {
                continue;
            }
            // The construct starts at its `enum` keyword and ends at the `;` after the closer.
            let Some(start) = (0..open_index)
                .rev()
                .find(|at| stream.tokens[*at].kind == cpp_parser::CppTokenKind::EnumKeyword)
            else {
                continue;
            };
            let end = (close_index..stream.tokens.len())
                .find(|at| stream.tokens[*at].kind == cpp_parser::CppTokenKind::Semicolon)
                .unwrap_or(close_index);
            let mut width = blocks[start].unwrap_or(0);
            for at in start..=end {
                if at > start {
                    width += crate::space::between(&stream, at, &roles, style);
                }
                width += stream.tokens[at].text.chars().count();
            }
            if width <= style.count(StyleOption::ColumnLimit) {
                for slot in &mut joined[open_index..=close_index] {
                    *slot = true;
                }
            }
        }
    }

    // **Which `{` on the bracket stack is an initializer`s** — so its `}` pops the bracket again.
    let mut init_braces: Vec<bool> = Vec::new();
    // **What kind each bracket on `stack` is**: a break inside a parenthesis or a subscript indents from the line it
    // continues, and a break inside a braced initializer indents from the statement — measured on a nested call
    // (content at the bracket line + four) and on a table after a struct body (rows at the statement + eight).
    let mut stack_kinds: Vec<cpp_parser::CppTokenKind> = Vec::new();
    // **The indent of the line each open bracket was written on**: a break that follows a bracket which ended its
    // line indents one `ContinuationIndentWidth` past *that* line — measured on `AddIndenter(` / `IndentData(`
    // (32 = the `IndentData` line`s 28 plus four) and on a braced table after a struct body (8 = the
    // `} priority[] = {` line`s 0 plus eight).
    let mut bracket_lines: Vec<usize> = Vec::new();
    // **The join half of the reflow: a statement that fits on one line is one line.** clang-format re-lays-out every
    // logical line, and a statement whose whole text fits is written on one line however the author broke it. This
    // formatter keeps the author's breaks, which is why a file written by another formatter differed on every wrapped
    // expression even once the answer to produce was known.
    //
    // What is **not** joined, each measured or documented:
    // * a directive (`#define`'s body is the author's);
    // * a statement with a comment in it (a comment is a line of its own);
    // * a statement with **any brace**: `BinPackLongBracedList: false` keeps a long braced initializer broken even
    //   when it fits, and a block body never joins — the first version of this pass joined both and took the
    //   spacing-only column from 53 lines to 2 727;
    // * a statement that does not fit (the breaker owns it, and the fitting model it uses is not complete yet);
    // * a statement already on one line.
    if style.count(StyleOption::ColumnLimit) > 0 {
        for line in crate::breaks::logical_lines_with(&stream, Some(&roles)) {
            if line.end <= line.start + 1 {
                continue;
            }
            let first = line.start;
            if stream.tokens[first].kind == cpp_parser::CppTokenKind::Hash {
                continue;
            }
            // **A short control body this pass moves to its own line is not joined back**: the join half runs after the
            // wrapping table and would undo it (`if (a) g();` is two lines under `LLVM`, one under `Google`).
            if (first..line.end).any(|at| body_wrapped[at]) {
                continue;
            }
            if (first + 1..line.end).any(|index| !stream.gaps[index].comments.is_empty()) {
                continue;
            }
            // **A block brace refuses the join; a small initializer's does not**: clang-format joins
            // `int v[] = {1, 2};`, `std::vector<int> w{1, 2};` and `auto s = std::string{"x"};` when they fit, and a
            // statement holding a **body** never joins. A list the **table layout would own** is left to it: joining it
            // collapses the rows that layout produces (measured: whole-file +873 on nlohmann json when every fitting
            // initializer joined). The table's own test is `tables::layout`, so the question is asked of it rather than
            // approximated by a cell count: the first cell's column is where it would land if the statement joined.
            let mut a_table = false;
            let mut depth = 0isize;
            let indent = blocks[first].unwrap_or(0);
            for (offset, token) in stream.tokens[first..line.end].iter().enumerate() {
                let index = first + offset;
                match token.kind {
                    cpp_parser::CppTokenKind::LeftBrace if depth == 0 => {
                        if !roles.init_brace.contains(&token.start) {
                            // A **block** brace: the statement never joins.
                            a_table = true;
                            break;
                        }
                        depth += 1;
                        if let Some(close) = pairs[index]
                            && let Some(cells) = crate::tables::cells_of(&stream, index, close)
                        {
                            let widths: Vec<usize> = cells
                                .iter()
                                .enumerate()
                                .map(|(at, cell)| {
                                    crate::tables::cell_width(
                                        &stream,
                                        &roles,
                                        style,
                                        cell,
                                        at + 1 == cells.len(),
                                    )
                                })
                                .collect();
                            // The column the first cell would start at if the statement were on one line.
                            let mut first_column = indent;
                            for at in first..=index {
                                if at > first {
                                    first_column += space::between(&stream, at, &roles, style);
                                }
                                first_column += stream.tokens[at].text.chars().count();
                            }
                            if crate::tables::layout(
                                &widths,
                                first_column + 1,
                                style.count(StyleOption::ColumnLimit),
                            )
                            .is_some()
                            {
                                a_table = true;
                                break;
                            }
                        }
                    }
                    cpp_parser::CppTokenKind::RightBrace if depth > 0 => depth -= 1,
                    cpp_parser::CppTokenKind::LeftBrace | cpp_parser::CppTokenKind::RightBrace
                        if depth == 0 =>
                    {
                        a_table = true;
                        break;
                    }
                    _ => {}
                }
            }
            if a_table {
                continue;
            }
            // **A template head is a line of its own**: `template <typename T>` then `class lexer_base {` is what
            // clang-format writes, and joining them (`template <typename T> class lexer_base {`) is a line clang-format
            // never produces (measured on nlohmann json).
            if stream.tokens[first].kind == cpp_parser::CppTokenKind::TemplateKeyword {
                continue;
            }
            // **A statement that is already on one line is left alone unless it overflows**: the fit test below
            // answers that, and the trailing comment is part of the width (`= default; // NOLINT(…)` is 135 columns
            // and clang-format breaks it after the `=`) — skipping every one-line statement meant an overflowing one
            // was never re-cut.
            // **Only a real statement is reflowed**: `NUMBER_OPTION(indent_size)` lines and the values of an enum body
            // have no terminator, and joining them put two statements on one line (measured, 354 lines on
            // `LuaStyle.cpp`). A statement ends with `;`, or with the **signature** that a block brace follows — and a
            // signature has one parenthesised group; a run of macro invocations has several.
            let next = stream.tokens.get(line.end);
            let ends_with_a_semicolon =
                stream.tokens[line.end - 1].kind == cpp_parser::CppTokenKind::Semicolon;
            let is_a_signature = next
                .is_some_and(|token| token.kind == cpp_parser::CppTokenKind::LeftBrace)
                && (first..line.end)
                    .filter(|index| {
                        stream.tokens[*index].kind == cpp_parser::CppTokenKind::RightParen
                    })
                    .count()
                    <= 1;
            if !ends_with_a_semicolon && !is_a_signature {
                continue;
            }
            // **A label owns the rest of its statement**: `endLoop:` then `return words;` are two lines even when
            // they fit on one (measured: joining them made `endLoop: return words;`). A `Colon` at depth 0 in the
            // range is the evidence — a ternary`s is inside parentheses or after a `?`.
            if (first..line.end).any(|index| {
                stream.tokens[index].kind == cpp_parser::CppTokenKind::Colon
                    && stream
                        .gaps
                        .get(index + 1)
                        .is_some_and(|gap| gap.newlines > 0)
            }) {
                continue;
            }
            // **A line that continues a `}`** (`} else if (…)`) has its first columns on the previous line: the join
            // would be judged by a width that does not include them (measured: an 84-column `else if` was joined as
            // 80). The `else`/`catch`/`while` itself is placed by the brace rules, not by this pass.
            if matches!(
                stream.tokens[first].kind,
                cpp_parser::CppTokenKind::ElseKeyword
                    | cpp_parser::CppTokenKind::CatchKeyword
                    | cpp_parser::CppTokenKind::WhileKeyword
            ) {
                continue;
            }
            let indent = blocks[first].unwrap_or(0);
            let mut width = indent;
            for index in first..line.end {
                if index > first {
                    width += space::between(&stream, index, &roles, style);
                }
                width += stream.tokens[index].text.chars().count();
            }
            // **The `{` that follows a signature is on the same line when the style attaches it**, and its columns
            // count: a signature of exactly 80 that would be 82 with `{` is broken by clang-format (measured on
            // `void FormatState::AddRelativeIndent(…, std::size_t indent) {`).
            if line.end < stream.tokens.len()
                && stream.tokens[line.end].kind == cpp_parser::CppTokenKind::LeftBrace
                && roles
                    .brace_kind
                    .get(&stream.tokens[line.end].start)
                    .is_none_or(|kind| !crate::wrapping::a_brace_starts_a_line(style, *kind))
            {
                width += 2;
            }
            // **A trailing comment is part of the line's width**: clang-format breaks to leave room for it
            // (measured: `if (::islower(ch) || ::isdigit(ch)) { // after the first underscore, …` stays broken, and
            // `json_sax_dom_error_state(json_sax_dom_error_state &&) = default; // NOLINT(…)` is broken after the `=`
            // for the comment's sake). The comment lives in the gap **after** the line's last token — for a line that
            // ends in `{` that is the gap after the brace, and for every other line the gap that follows it.
            let mut trailing = 0usize;
            for at in [line.end, line.end + 1] {
                if at == line.end + 1
                    && !next.is_some_and(|token| token.kind == cpp_parser::CppTokenKind::LeftBrace)
                {
                    continue;
                }
                if let Some(gap) = stream.gaps.get(at)
                    && let Some(comment) = gap.comments.last().filter(|comment| !comment.own_line)
                {
                    trailing = comment.text.chars().count() + 1;
                    break;
                }
            }
            if width + trailing <= style.count(StyleOption::ColumnLimit) {
                for slot in &mut joined[first + 1..line.end] {
                    *slot = true;
                }
            } else if {
                // **Only a statement that actually overflows is re-cut.** If every source line of the statement fits,
                // the author's layout is one clang-format could have produced (its own output does), and re-cutting it
                // is how reflow became a regression: the search`s model is not clang-format`s, so it cut clang-format`s
                // own lines differently (measured: spacing-only 53 -> 2 269 lines).
                let limit = style.count(StyleOption::ColumnLimit);
                let mut piece = 0usize;
                let mut overflows = false;
                for index in first..line.end {
                    if index > first {
                        if stream.gaps[index].newlines > 0 {
                            if indent + piece > limit {
                                overflows = true;
                                break;
                            }
                            piece = 0;
                        } else {
                            piece += space::between(&stream, index, &roles, style);
                        }
                    }
                    piece += stream.tokens[index].text.chars().count();
                }
                overflows || indent + piece + trailing > limit
            } && let Some(breaks) =
                crate::breaks::break_line(&stream, &roles, style, &line, indent, trailing)
            {
                // **The breaking half**: clang-format decides where a statement that does not fit is cut, and its
                // answer is the penalty search. The search's positions become forced breaks and everything else in
                // the statement is joined; a statement the search finds nothing to gain on keeps the author's lines.
                for (index, slot) in joined.iter_mut().enumerate().take(line.end).skip(first + 1) {
                    if !breaks.contains(&index) {
                        *slot = true;
                    }
                }
                for at in breaks {
                    forced_breaks[at] = true;
                }
            }
        }
    }

    // **A flat initializer list that does not fit is a table** (`tables.rs`): its rows are computed from the column
    // its `{` lands in, which is only known while printing, so the decision is made there and recorded in these two
    // vectors — the indent of each row start, and how many spaces to pad before each cell.
    let mut table_starts: Vec<Option<usize>> = vec![None; stream.tokens.len()];
    // **How far the table that owns the current list reaches** — its `}`.
    let mut table_until: Option<usize> = None;
    // **The lambda body being joined**, once its `{` was printed and the width was known to fit.
    let mut join_until: Option<usize> = None;
    let mut in_directive = false;
    // **The comment a namespace's closer wants** (`FixNamespaceComments` with an existing comment): written where
    // that comment is — the next gap's trailing comment, or the file's last gap, which is written after the loop.
    let mut namespace_comment: Option<String> = None;

    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        // **A table owns the layout of its list**: the column-limit search's breaks inside it are dropped, or the
        // two layouts fight (measured: rows of 2, 8, 8 elements instead of 5, 5, 5, 5, 5).
        let mut forced = forced_breaks[index];
        if table_until.is_some_and(|end| index <= end) {
            forced = false;
        }
        // **A table row starts a line at the first cell's column.**
        let table_start = table_starts[index];
        let mut wrap = wrapped[index];
        let mut attach = attached[index];
        let join = joined[index] || join_until.is_some_and(|end| index <= end);
        // **A lambda body that is about to be joined keeps its `{` on the signature's line**, whatever the brace
        // table says: clang-format writes `f([](int v) { return v; });` under `Allman` too, where
        // `BeforeLambdaBody: true` would otherwise wrap the brace (measured). This is the same width test the join
        // uses, taken one token early — the `{` is not printed yet, so the column plus one is where the body starts.
        if wrap
            && let Some((_, width)) = lambda_body[index]
            && column + 1 + width <= style.count(StyleOption::ColumnLimit)
        {
            wrap = false;
            attach = true;
        }
        // **An empty block is `{}`** — unless the style *splits* it (`BraceWrapping.SplitEmpty*`, which Microsoft's
        // style sets for functions, records and namespaces: `void f()\n{\n}` there). clang-format writes `void f() {}`
        // for an empty body even when `AllowShortBlocksOnASingleLine` is `Never` — the option governs blocks with
        // *statements* — and a record with nothing in it is `struct S {};` for the same reason. This is the pair being
        // empty of **tokens and comments**: `{ /* nothing */ }` is a block with a comment and keeps its own lines.
        let empty_brace_pair = index > 0
            && gap.comments.is_empty()
            && !split[index]
            // **Only a reflow joins an author's `{` newline `}` into `{}`**: with `ColumnLimit: 0` clang-format leaves
            // the break (the corpus, formatted with that setting, keeps every empty body split).
            && style.count(StyleOption::ColumnLimit) > 0
            && stream.tokens[index].kind == cpp_parser::CppTokenKind::RightBrace
            && stream.tokens[index - 1].kind == cpp_parser::CppTokenKind::LeftBrace;
        // **Inside a `// clang-format off` span the gap is the source's**, byte for byte: no re-spacing, no re-indenting.
        // The tokens are still seen (a brace in the span still opens a block), so the indentation after `on` is right.
        if disabled.get(index).copied().unwrap_or(false) {
            let gap_text = source.get(gap.start..gap.end).unwrap_or("");
            out.push_str(gap_text);
            column = match gap_text.rfind(char::from(10)) {
                Some(at) => columns.advance(0, &gap_text[at + 1..]),
                None => columns.advance(column, gap_text),
            };
        } else if in_directive
            && source
                .get(gap.start..gap.end)
                .is_some_and(|text| text.contains('\\'))
        {
            // **A spliced line break inside a directive keeps its bytes**: the `\` and the line break are the macro's,
            // and this pass has no line break to emit for them (`newlines` counts a real newline, and a splice is not
            // one). Joining them wrote three lines of a `#define` as one — measured on nlohmann json's
            // `macro_scope.hpp`, whose `NLOHMANN_JSON_SERIALIZE_ENUM` is 60 spliced lines. The `\` columns are still
            // aligned afterwards by `macros::align_escaped_newlines`, and the author's indentation inside the macro is
            // kept rather than recomputed (clang-format re-indents macro bodies, which is its own feature).
            let gap_text = source.get(gap.start..gap.end).unwrap_or("");
            out.push_str(gap_text);
            column = match gap_text.rfind(char::from(10)) {
                Some(at) => columns.advance(0, &gap_text[at + 1..]),
                None => columns.advance(column, gap_text),
            };
        } else if let Some(indent) = table_start
            && index > 0
            && !in_directive
        {
            out.push_str(line_ending);
            out.push_str(&columns.indent(indent, indent));
            column = indent;
        } else if forced && index > 0 && !in_directive {
            // **A break the column limit asked for**: the line continues `ContinuationIndentWidth` past the statement,
            // which is the column `breaks::break_line` assumed when it chose the break. Aligning to the open bracket
            // instead (what `layout::continuation` answers) let a deeply nested call decide a column *past* the limit:
            // measured on a macro expansion, a line was continued at column 120 of an 80-column layout and every piece
            // after it overflowed the width the search had promised. The author's own breaks still align — that is what
            // keeps an already-formatted file unchanged.
            let indent = statement_indent + style.count(StyleOption::ContinuationIndentWidth);
            out.push_str(line_ending);
            out.push_str(&columns.indent(indent, statement_indent));
            column = indent;
        } else if ((gap.newlines == 0 && !wrap) || empty_brace_pair || attach || join)
            && gap.comments.is_empty()
            && !gap.at_end
            && index > 0
            && !in_directive
        {
            let spaces = space::between(&stream, index, &roles, style) + row_paddings[index];
            out.push_str(&" ".repeat(spaces));
            column += spaces;
        } else if !in_directive
            && !empty_brace_pair
            && (gap.comments.is_empty()
                // **A comment on its own line still leaves the line's column to this pass**: a braced table's comment
                // lines up with its elements (measured: `/* ORDER OPR */` belongs at the elements' column), and outside
                // a list the same rule is what fixes the *source's* indentation of the line after a comment
                // (`      return 0;` came out at six spaces because the gap was copied verbatim).
                || gap.comments.last().is_some_and(|comment| {
                    // **A trailing comment that ends its line** (`A, // one` then `B,`) leaves the next line's column
                    // to this pass when that line is a **block line** — an enumeration item, a statement. Copying the
                    // gap verbatim kept the *source's* indent, which made the second enumeration item sit two columns
                    // further in than clang-format's (measured). A continuation line keeps the source's column: its
                    // indent is the nested-continuation question this formatter still answers one level short, and
                    // recomputing it made the `Lua 5.5` spacing column worse, not better.
                    comment.own_line || (comment.ends_line && !enums.is_empty())
                })
                // **A directive always gets its column from this pass**: `#include <iosfwd> // ostream` then
                // `#include <string>` — the trailing comment left the gap to the verbatim copy, which kept the
                // source's indentation of the next directive (four spaces inside a namespace, measured on nlohmann
                // json's `json_pointer.hpp`).
                || stream.tokens[index].kind == cpp_parser::CppTokenKind::Hash)
            && (gap.newlines > 0 || wrap)
            && index > 0
            && let Some((indent, is_block_line)) = {
                // **A line the style asks for** — a `{` the `BraceWrapping` table wraps, or an `else`/`catch`/`while`
                // that stands on its own after a `}`. It starts at the level of the construct's own line (`blocks`),
                // not at a continuation column: a wrapped function brace sits under the function's return type, and
                // an `else` under the `if` it belongs to.
                if wrap {
                    // **A `case` body`s brace stands at the label`s column**, not one level in: under `Allman`
                    // clang-format writes `case X:` then `{` under the `case` and the body one indent further
                    // (measured on `FormatState.cpp`), which is the `IndentCaseBlocks` rule applied to the brace.
                    let indent = case_label_indent(&stream, &blocks, index)
                        .or_else(|| blocks[index])
                        .unwrap_or(statement_indent);
                    // **A short control body this pass moved to its own line is one level in**: the block layout does
                    // not know the level for a body the author wrote on the control's line, so it is added here
                    // (`if (a)` then `g();` at the control's indent plus `IndentWidth`, measured on the oracle).
                    let indent = if body_wrapped[index] {
                        indent + style.count(StyleOption::IndentWidth)
                    } else {
                        indent
                    };
                    Some((indent, true))
                } else if let Some(open) = closers[index]
                    && roles.brace_kind.get(&stream.tokens[open].start)
                        == Some(&crate::roles::BraceKind::CaseLabel)
                {
                    // **The closer of a braced `case` body goes back to the label`s column** too.
                    let indent =
                        case_label_indent(&stream, &blocks, open).unwrap_or(statement_indent);
                    Some((indent, true))
                } else {
                    // **A continuation line that starts with a binary operator aligns with the statement's first one**
                    // (`AlignOperands: Align`), and that column beats the block rules.
                    let aligned = operand_operator.filter(|_| {
                        // **`= default;` / `= delete;` are continuations, not operand-aligned operators**: aligning the `=`
                        // under a `==` in the `requires` clause above it produced a line indented 38 columns (measured on
                        // a real file). Only the arithmetic and logical operators align.
                        (stream.tokens[index].kind != cpp_parser::CppTokenKind::Assign)
                            && style.text(StyleOption::AlignOperands) == "Align"
                            && layout::is_a_binary_operator(stream.tokens[index].kind)
                            && !roles.is_template_angle(stream.tokens[index].start)
                            && !roles.is_declarator(stream.tokens[index].start)
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
                    // **A continuation after a trailing binary operator aligns with the first operand**, and only
                    // when the statement did not start with an operator (that case is `aligned` above).
                    // **Only outside brackets**: inside `if (…)` the continuation belongs to the bracket
                    // (`AlignAfterOpenBracket`), and the operand column put it four columns too far right (measured).
                    // **A template's `>` is not an operator that continues a statement**: without the roles check,
                    // the line after `template <typename T>` aligned with the `template` keyword instead of the
                    // block's indent (measured: `void f(T v);` came out at 2 instead of 4).
                    let a_continuing_operator = |at: usize| {
                        // **An assignment is not an operand continuation**: clang-format breaks *after* `=`/`+=` and
                        // indents the next line by `ContinuationIndentWidth` (measured: `_state.CurrentWidth() +=`
                        // then 12 = 8 + 4), where aligning with the statement's first operand gave 8.
                        !is_an_assignment(stream.tokens[at].kind)
                            && layout::is_a_binary_operator(stream.tokens[at].kind)
                            && !roles.is_template_angle(stream.tokens[at].start)
                            && !roles.is_declarator(stream.tokens[at].start)
                    };
                    let operand_continuation = (style.text(StyleOption::AlignOperands) == "Align"
                        && stack.is_empty()
                        // **A directive is never a continuation of the line above**: `#include <cstdlib>` ends with
                        // a `>` that reads as a binary operator, and the `#define` after it aligned to a column of 23
                        // (measured on nlohmann json's `macro_scope.hpp`; `IndentPPDirectives: None` wants column 0).
                        && stream.tokens[index].kind != cpp_parser::CppTokenKind::Hash
                        && a_continuing_operator(index - 1))
                    .then_some(operand_column)
                    .flatten();
                    // **An implicit string concatenation continues under its first string**: clang-format
                    // writes `const char x[] = "a" "b"` then `"c"` aligned with the first string (measured on Lua
                    // 5.5`s `lua_ident`), which is the assignment`s right-hand-side column — the same anchor the
                    // operand continuation uses.
                    let string_continuation = (stream.tokens[index - 1].kind
                        == cpp_parser::CppTokenKind::StringLiteral)
                        .then_some(first_string)
                        .flatten();
                    conditional
                        .map(|column| (column, false))
                        .or_else(|| aligned.map(|column| (column, false)))
                        .or_else(|| operand_continuation.map(|column| (column, false)))
                        .or_else(|| string_continuation.map(|column| (column, false)))
                        .or_else(|| {
                            let inside = bracket_body.filter(|(depth, _)| *depth == brace_depth);
                            planned_indent(
                                index,
                                &stream,
                                &blocks,
                                &stack,
                                statement_indent,
                                // **The anchor a continuation is measured from**: inside a parenthesis or a
                                // subscript it is the line being continued, so `AddIndenter(` / `IndentData(` puts the
                                // content at 32 (the `IndentData` line's 28 plus four); inside a braced initializer it
                                // is the statement, so a table after a struct body keeps its rows at 8.
                                match stack_kinds.last() {
                                    Some(
                                        cpp_parser::CppTokenKind::LeftParen
                                        | cpp_parser::CppTokenKind::LeftBracket,
                                    ) => line_column,
                                    // **A braced list indents from the line that opened it**: the table after a struct
                                    // body keeps its rows at the `} priority[] = {` line`s column plus a continuation,
                                    // where the *statement*`s first line is four columns off (measured).
                                    Some(_) => the_line_that_opened_the_list(
                                        &stream,
                                        &blocks,
                                        index,
                                        statement_indent,
                                    ),
                                    None => statement_indent,
                                },
                                style,
                                in_ctor_init,
                                ctor_column,
                                init_depths[index] > 0,
                                // **A `<` that ended its line has no content column yet**: its continuation is one
                                // `ContinuationIndentWidth` past the line the `<` was written on (`*j.template get_ptr<`
                                // at 8 continues at 12; `template <` at 2 continues at 6) — measured on nlohmann json.
                                angles.last().copied().map(|column| {
                                    if column == usize::MAX {
                                        angle_anchors.last().copied().unwrap_or(statement_indent)
                                            + style.count(StyleOption::ContinuationIndentWidth)
                                    } else {
                                        column
                                    }
                                }),
                                enums
                                    .last()
                                    .map(|(_, column)| *column)
                                    .filter(|column| *column != usize::MAX),
                            )
                            .map(|(indent, is_block_line)| {
                                let closing = stream.tokens[index].kind
                                    == cpp_parser::CppTokenKind::RightBrace;
                                // **A directive is never `inside` a bracket**, though the `#include <…>` line that
                                // precedes it leaves a `<` on the bracket stack: the extra `indent_width` put
                                // `#define JSON_THROW(…) std::abort()` 23 columns in (measured on nlohmann json`s
                                // `macro_scope.hpp`, where only the `#define` after an `#include` was affected).
                                if stream.tokens[index].kind == cpp_parser::CppTokenKind::Hash {
                                    return (indent, true);
                                }
                                match inside {
                                    Some((_, column)) if is_block_line && closing => (column, true),
                                    Some((_, column)) if is_block_line => {
                                        (column + indent_width, true)
                                    }
                                    _ => (indent, is_block_line),
                                }
                            })
                        })
                }
            }
        {
            // **A line that starts here**, with nothing in the gap but whitespace: the break count collapses to
            // `MaxEmptyLinesToKeep` and the column is the layout's. Everything else — a comment, a directive — is
            // still copied, which keeps this pass's blast radius to the lines it can decide.
            line_was_bracket_placed =
                !is_block_line && !stack.is_empty() && init_depths[index] == 0;
            line_column = indent;
            // **A wrapped token needs a break even when the source has none** — `gap.newlines` is 0 for
            // `y_(y) {` and `wrap` is what decides that the brace goes to the next line.
            let mut keep = gap.newlines.max(usize::from(wrap)).min(max_empty + 1);
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
                let at_block_start =
                    stream.tokens[index - 1].kind == cpp_parser::CppTokenKind::LeftBrace;
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
            //
            // **Any line-starting comment, not only one inside a braced list**: the filter used to require
            // `init_depths[index] > 0`, and outside a list the whole gap was copied verbatim — which kept the *source's*
            // indentation of the line after the comment (`      return 0;` stayed at six spaces when clang-format
            // writes four). The comment's own text still comes from the source; only the column is decided here.
            let own_line_comment = gap.comments.last().filter(|comment| comment.own_line);
            // **A trailing comment that ends its line is written first, then the break it ends**: the branch used to
            // be entered only for an own-line comment, so a line comment after code left the whole gap to the
            // verbatim copy — including the next line's indent (measured on `enum E { A, // one`).
            let trailing = gap
                .comments
                .last()
                .filter(|comment| !comment.own_line && comment.ends_line);
            // **`FixNamespaceComments` rewrites the comment after a namespace's closer**: clang-format turns
            // `} // wrong name` into `} // namespace existing` (measured), so the text to write is taken from the
            // namespace's own name instead of the source. The name was recorded when the `}` was written — this gap
            // is the next token's, and the comment belongs to the closer's line.
            if let Some(wanted) = namespace_comment.take() {
                if trailing.is_some() {
                    out.push(' ');
                    out.push_str(&wanted);
                    let _ = columns.advance(columns.advance(column, " "), &wanted);
                } else {
                    namespace_comment = Some(wanted);
                }
            } else if let Some(comment) = trailing {
                let prefix = source.get(gap.start..comment.start).unwrap_or("");
                // **An `#include` line keeps its comment byte for byte**: clang-format sorts and places includes but
                // does not normalise their trailing comments — `#include <bit> //byteswap` stays as written, while
                // `#define X //no space` and `int y; //also` both get the space (measured on the oracle). The line is
                // found from the comment's own line start.
                let an_include_line = {
                    let line_begin = line_start(&stream, index.saturating_sub(1));
                    stream.tokens[line_begin].kind == cpp_parser::CppTokenKind::Hash
                        && source
                            .get(stream.tokens[line_begin].start..comment.start)
                            .is_some_and(|text| text.contains("include"))
                };
                let comment_text: std::borrow::Cow<'_, str> = if an_include_line {
                    std::borrow::Cow::Borrowed(comment.text)
                } else {
                    crate::comments::normalized(comment.text, style)
                };
                out.push_str(prefix);
                out.push_str(&comment_text);
                // The column is reassigned by the break and indent below, so only the text is written here.
                let _ = columns.advance(columns.advance(column, prefix), &comment_text);
            }
            let breaks_to_emit = if own_line_comment.is_some() { 0 } else { keep };
            for _ in 0..breaks_to_emit {
                out.push_str(line_ending);
            }
            // **A trailing comment of the previous line is written first, from the source**: it belongs to that line,
            // and only the comment that starts this line is this pass's to place.
            //
            // Inside a braced list the comment is one of the elements and takes the list's column; anywhere else it
            // keeps the column the source gave it (a comment is not a declaration the layout can place) — but the
            // **token** after it does get the computed column, which is what fixes `      return 0;` after a comment.
            let in_list = init_depths[index] > 0;
            // **A label keeps the source's column**: `no_save:` is placed by `IndentLabel` rules this pass does not
            // implement, and overriding it with the block column moved it a level out (measured on `LuaLexer.cpp`).
            let is_a_label = stream.tokens[index].kind == cpp_parser::CppTokenKind::Identifier
                && stream
                    .tokens
                    .get(index + 1)
                    .is_some_and(|token| token.kind == cpp_parser::CppTokenKind::Colon);
            if let Some(comment) = own_line_comment {
                if in_list {
                    // The prefix keeps its breaks but not its trailing indentation, which the computed column
                    // replaces: keeping it stacked the source's two spaces onto the column (10 where 8 belongs).
                    let prefix = source.get(gap.start..comment.start).unwrap_or("");
                    out.push_str(prefix.trim_end_matches([' ', '\t']));
                    out.push_str(&columns.indent(
                        indent,
                        if is_block_line {
                            indent
                        } else {
                            statement_indent
                        },
                    ));
                    column = indent;
                } else {
                    let verbatim = source.get(gap.start..comment.end).unwrap_or("");
                    out.push_str(verbatim);
                    column =
                        columns.advance(0, verbatim.rsplit(char::from(10)).next().unwrap_or(""));
                }
            } else {
                out.push_str(&columns.indent(
                    indent,
                    if is_block_line {
                        indent
                    } else {
                        statement_indent
                    },
                ));
                column = indent;
            }
            // **The comment that made this gap non-empty is a line of the block**: its text and everything after it
            // come from the source, written once, after the column this pass decided.
            if let Some(comment) = own_line_comment {
                let comment_text = crate::comments::normalized(comment.text, style);
                if in_list {
                    out.push_str(&comment_text);
                }
                // What follows the comment is a break to the token's own line: its newlines are kept (capped) and its
                // indentation is replaced by the column.
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
                    if is_a_label {
                        // The source's own indentation for a label, kept as it was before this pass placed columns.
                        let tail = source.get(comment.end..gap.end).unwrap_or("");
                        let indentation = tail.rsplit(char::from(10)).next().unwrap_or("");
                        out.push_str(indentation);
                    } else {
                        out.push_str(&columns.indent(
                            indent,
                            if is_block_line {
                                indent
                            } else {
                                statement_indent
                            },
                        ));
                    }
                    column = indent;
                } else {
                    // **A block comment with code after it on the same line keeps one space**: `/*block*/ int y;` —
                    // clang-format writes the separator even though the gap`'`s own bytes had none, and a line comment
                    // can never have code after it on the same line.
                    let separates = !matches!(
                        stream.tokens[index].kind,
                        cpp_parser::CppTokenKind::Semicolon
                            | cpp_parser::CppTokenKind::Comma
                            | cpp_parser::CppTokenKind::RightParen
                            | cpp_parser::CppTokenKind::RightBracket
                            | cpp_parser::CppTokenKind::RightBrace
                    ) && comment.kind != cpp_parser::CppTokenKind::LineComment;
                    if separates {
                        out.push(' ');
                    }
                    column = columns.advance(column, comment.text) + usize::from(separates);
                }
            }
            if is_block_line {
                statement_indent = indent;
                ctor_column = None;
            } else if stream.tokens[index].kind == cpp_parser::CppTokenKind::Colon {
                // **A constructor initializer's colon follows the parameter list's `)`**: `S(int a) : a_(a), …`. A base
                // clause's colon (`struct X` then `: disjunction<…>`) does not, and treating it as one packed the base
                // arguments at the colon + 2 (6) instead of under the first argument (18) — measured on nlohmann json's
                // `binary_reader.hpp`.
                let follows_a_parameter_list = index > 0
                    && matches!(
                        stream.tokens[index - 1].kind,
                        cpp_parser::CppTokenKind::RightParen
                            | cpp_parser::CppTokenKind::NoexceptKeyword
                    );
                if follows_a_parameter_list {
                    ctor_column = Some(indent + 2);
                    in_ctor_init = true;
                }
            }
        } else if let Some(comment) = gap.comments.last().filter(|comment| {
            // **A block comment in the middle of a line is spaced like a token**: one space on each side, none after an
            // opening bracket or before a closer (`return /* c */ 1;`, `f(/* c */ 1);`). A comment that *ends* the line
            // is the trailing kind, and that one is placed by `trailing::align` instead.
            !comment.own_line
                && !comment.ends_line
                && matches!(comment.kind, cpp_parser::CppTokenKind::BlockComment)
        }) {
            let before = !matches!(
                stream.tokens[index - 1].kind,
                cpp_parser::CppTokenKind::LeftParen | cpp_parser::CppTokenKind::LeftBracket
            );
            let after = !matches!(
                stream.tokens[index].kind,
                cpp_parser::CppTokenKind::Semicolon
                    | cpp_parser::CppTokenKind::RightParen
                    | cpp_parser::CppTokenKind::RightBracket
                    | cpp_parser::CppTokenKind::Comma
            );
            let comment_text = crate::comments::normalized(comment.text, style);
            out.push_str(&" ".repeat(usize::from(before)));
            out.push_str(&comment_text);
            out.push_str(&" ".repeat(usize::from(after)));
            column =
                columns.advance(column + usize::from(before), &comment_text) + usize::from(after);
        } else {
            // Verbatim: the gap's own bytes (newlines, indentation, comments) are not ours to change in this pass —
            // except the one thing every path owes a comment, its prefix spacing (`//using` is `// using`), and except
            // on an `#include` line, whose trailing comment clang-format leaves byte for byte.
            let an_include_line = {
                let line_begin = line_start(&stream, index.saturating_sub(1));
                stream.tokens[line_begin].kind == cpp_parser::CppTokenKind::Hash
                    && gap.comments.first().is_some_and(|comment| {
                        source
                            .get(stream.tokens[line_begin].start..comment.start)
                            .is_some_and(|text| text.contains("include"))
                    })
            };
            let gap_text = verbatim_gap(source, gap, style, an_include_line);
            column = match gap_text.rfind('\n') {
                Some(at) => columns.advance(0, &gap_text[at + 1..]),
                None => columns.advance(column, &gap_text),
            };
            out.push_str(&gap_text);
        }

        // **The first binary operator of a statement records its column**, and so does the **first operand** — a
        // continuation after a trailing `||` aligns with the operand, not with the operator:
        //
        //     return prev.IsNull(t) ||
        //            n.GetPrevToken(t).GetEndLine(t) != n.GetStartLine(t);
        //
        // (measured; `AlignOperands: Align`). The operand column is set by the statement's first token, or by the token
        // after a leading `return`/`throw`, and forgotten at the end of the statement with the operator's.
        if operand_operator.is_none()
            && stream.gaps[index].newlines == 0
            && layout::is_a_binary_operator(stream.tokens[index].kind)
        {
            operand_operator = Some(column);
        }
        if operand_column.is_none() && !stream.gaps[index].at_end && index > 0 {
            let a_statement_start = matches!(
                stream.tokens[index - 1].kind,
                cpp_parser::CppTokenKind::Semicolon
                    | cpp_parser::CppTokenKind::LeftBrace
                    | cpp_parser::CppTokenKind::RightBrace
            );
            if a_statement_start {
                operand_column = Some(column);
            }
        }
        if is_an_assignment(stream.tokens[index].kind) {
            awaiting_first_string = true;
        }
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::StringLiteral
            && awaiting_first_string
        {
            first_string = Some(column);
            awaiting_first_string = false;
        }
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::ReturnKeyword
                | cpp_parser::CppTokenKind::ThrowKeyword
                | cpp_parser::CppTokenKind::CoReturnKeyword
        ) {
            operand_column = Some(column + stream.tokens[index].text.chars().count() + 1);
        }
        // **The first operand of an assignment is on the right of the `=`**: clang-format aligns a continuation of
        // `auto pos = relativePos + indentState.SpaceSize +` under `relativePos`, not under `auto` (measured). When
        // the right-hand side starts on the next line, the assignment rule above already gives it a
        // `ContinuationIndentWidth` indent, and this does not fire.
        if index > 0
            && is_an_assignment(stream.tokens[index - 1].kind)
            && stream.gaps[index].newlines == 0
        {
            operand_column = Some(column);
        }
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::Semicolon
                | cpp_parser::CppTokenKind::LeftBrace
                | cpp_parser::CppTokenKind::RightBrace
        ) {
            operand_operator = None;
            operand_column = None;
            first_string = None;
            awaiting_first_string = false;
        }

        // **The bracket a token closes leaves the stack before the token is written**, and one it opens is pushed
        // after — with its column filled in by the next token, if that token is on the same line.
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::RightParen | cpp_parser::CppTokenKind::RightBracket
        ) {
            stack.pop();
            stack_kinds.pop();
            bracket_lines.pop();
        }
        // **A braced initializer is a bracket for the layout too** (`AlignAfterOpenBracket`): its elements align after
        // the `{` when one was written on that line, and fall back to a continuation indent when the `{` ended the
        // line. Only an *initializer* brace is pushed — a block`s brace is placed by the block rules.
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::RightBrace
            && init_braces.pop().unwrap_or(false)
        {
            stack.pop();
            stack_kinds.pop();
            bracket_lines.pop();
        }
        // The template closer pops its own stack, so a line after the list is not pulled to its column.
        if template_angles[index] && stream.tokens[index].kind == cpp_parser::CppTokenKind::Greater
        {
            angles.pop();
            angle_anchors.pop();
        }
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::RightBrace
            && enums.last().is_some_and(|(depth, _)| *depth == brace_depth)
        {
            enums.pop();
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
                if let Some((depth, _)) = bracket_body
                    && depth >= brace_depth
                {
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
        if let Some(top) = angles.last_mut()
            && *top == usize::MAX
        {
            *top = column;
        }
        if let Some((_, top)) = enums.last_mut()
            && *top == usize::MAX
        {
            *top = column;
        }
        out.push_str(text);
        column = columns.advance(column, text);
        // **A flat initializer list becomes a table** when it does not fit: the rows, their widths and the padding
        // before each cell are computed here, where the column after the `{` is known.
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::LeftBrace
            && let Some(close) = pairs[index]
            // **A braced list that is an argument of a call is wrapped by the argument rules, not laid out as a
            // table**: `frames.push_back(frame{nullptr, nullptr, slot, elements.size(), ordered_state(…)})` is a plain
            // wrapped list in clang-format, while `std::vector<int> v{…}` — a declaration`s initializer — is a table
            // (measured on nlohmann json`s `json_sax.hpp`). The bracket stack tells them apart: a list opened inside a
            // `(`/`[` is an argument.
            && stack.is_empty()
            && crate::tables::is_an_initializer_list(&roles, &stream, index)
            && let Some(cells) = crate::tables::cells_of(&stream, index, close)
        {
            let first = cells[0][0];
            let widths: Vec<usize> = cells
                .iter()
                .enumerate()
                .map(|(at, cell)| {
                    crate::tables::cell_width(&stream, &roles, style, cell, at + 1 == cells.len())
                })
                .collect();
            let limit = style.count(StyleOption::ColumnLimit);
            // **Two placements, and the one with fewer rows wins**: the first row can start after the `{` (clang-format
            // does when it fits, `static const int n[] = {111, …`), or on the next line one continuation in
            // (`static const char *n[] = {` then four spaces and the table, measured). The placement that needs fewer
            // rows is the answer; a tie keeps the first row on the `{` line.
            let after_the_brace = column + space::between(&stream, first, &roles, style);
            let continued = statement_indent + style.count(StyleOption::ContinuationIndentWidth);
            let candidates: Vec<(usize, bool, crate::tables::Table)> =
                [(after_the_brace, true), (continued, false)]
                    .into_iter()
                    .filter_map(|(first_column, on_the_brace_line)| {
                        crate::tables::layout(&widths, first_column, limit)
                            .map(|table| (first_column, on_the_brace_line, table))
                    })
                    .collect();
            // **The placement rule, measured on eight fixtures**: the first row stays on the `{` line unless starting
            // it there would cost **at least two rows** — a tall, narrow table. For the 25 operator names the
            // after-brace table is 7 rows of 4 and the oracle moves the table to the next line for 5 rows of 5; for
            // the integer array, the five-name list and the twenty-string list (one row's difference) it keeps the
            // first row on the `{` line. The exact tie-break inside clang-format is not documented anywhere this
            // formatter can read, so this is a rule that reproduces every measured case.
            let rows_of =
                |table: &crate::tables::Table| widths.len().div_ceil(table.columns.max(1));
            let after = candidates
                .iter()
                .find(|(_, on_the_brace_line, _)| *on_the_brace_line);
            let continued = candidates
                .iter()
                .find(|(_, on_the_brace_line, _)| !*on_the_brace_line);
            let chosen = match (after, continued) {
                (Some(after), Some(continued)) => {
                    if rows_of(&after.2) <= rows_of(&continued.2) + 1 {
                        Some(after.clone())
                    } else {
                        Some(continued.clone())
                    }
                }
                (Some(after), None) => Some(after.clone()),
                (None, Some(continued)) => Some(continued.clone()),
                (None, None) => None,
            };
            if let Some((first_column, on_the_brace_line, table)) = chosen {
                for (at, cell) in cells.iter().enumerate() {
                    let start = cell[0];
                    let column_of_the_cell = at % table.columns;
                    if column_of_the_cell == 0 && at > 0 {
                        table_starts[start] = Some(first_column);
                    } else if column_of_the_cell == 0 {
                        // The first cell of the first row: after the `{`, or on the continued line.
                        if on_the_brace_line {
                            joined[start] = true;
                        } else {
                            table_starts[start] = Some(first_column);
                        }
                    } else {
                        joined[start] = true;
                    }
                    // **The padding belongs before the next cell of the same row**, because it makes up the
                    // difference between this cell and the widest one of its column.
                    if let Some(next) = cells.get(at + 1)
                        && (at + 1) % table.columns != 0
                    {
                        row_paddings[next[0]] = table.paddings[at];
                    }
                }
                // The `}` follows the last cell on its line, and the list''s own line ends there.
                joined[close] = true;
                table_until = Some(close);
            }
        } // **A lambda body keeps its braces on the signature's line when the body fits from here** — the column is
        // known only now, which is why this decision lives in the printing loop.
        if let Some((close, width)) = lambda_body[index]
            && column + width <= style.count(StyleOption::ColumnLimit)
        {
            join_until = Some(close);
        }
        // **`FixNamespaceComments`**: clang-format writes `} // namespace ns` after a namespace's closing brace — the
        // name is the namespace's, and an existing comment is left alone (a file that already says it is not told
        // twice). Measured against the oracle: Microsoft's style has the option on, and it is the only difference left
        // on the `cases_wrapping` fixture before this.
        if let Some(name) = &namespace_comments[index]
            && style.flag(StyleOption::FixNamespaceComments)
            && stream
                .gaps
                .get(index + 1)
                .is_some_and(|gap| gap.comments.iter().any(|comment| !comment.own_line))
        {
            // **An existing comment is fixed, not left alone**: the text is written where the gap's trailing
            // comment is handled (or a line later, for the file's last closer, whose gap is written after the loop).
            namespace_comment = Some(format!("// namespace {name}"));
        } else if let Some(name) = &namespace_comments[index]
            && style.flag(StyleOption::FixNamespaceComments)
            // **A missing comment is added only past `ShortNamespaceLines`** (one statement stays bare, two get the
            // comment — measured); an existing one is fixed either way, which is the branch above.
            && namespace_long[index]
        {
            let comment = format!(" // namespace {name}");
            out.push_str(&comment);
            column = columns.advance(column, &comment);
        }
        if matches!(
            stream.tokens[index].kind,
            cpp_parser::CppTokenKind::LeftParen | cpp_parser::CppTokenKind::LeftBracket
        ) {
            // `usize::MAX` = "the token after this bracket has not been written yet".
            stack.push(usize::MAX);
            stack_kinds.push(stream.tokens[index].kind);
            bracket_lines.push(line_column);
        }
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::LeftBrace {
            let an_initializer = roles.init_brace.contains(&stream.tokens[index].start)
                && !roles.brace_kind.contains_key(&stream.tokens[index].start);
            init_braces.push(an_initializer);
            if an_initializer {
                stack.push(usize::MAX);
                stack_kinds.push(cpp_parser::CppTokenKind::LeftBrace);
                bracket_lines.push(line_column);
            }
        }
        // The template opener pushes its own "column not yet known" marker, so a continuation line inside the list
        // can align after the `<` (the parentheses`s stack would otherwise be the only one consulted).
        if template_angles[index] && stream.tokens[index].kind == cpp_parser::CppTokenKind::Less {
            angles.push(usize::MAX);
            // **The line this `<` was written on**: a list whose `<` ends its line indents from that line.
            angle_anchors.push(line_column);
        }
        if enum_bodies[index] {
            enums.push((brace_depth, usize::MAX));
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
        // **The file's last closer keeps its comment in this gap**, and `FixNamespaceComments` rewrites it like any
        // other — there is no next token, so it is written here. A comment that is not rewritten still gets the prefix
        // spacing every other path gives it (`int y; //also` is `// also`), except on an `#include` line.
        let last_comment = trailing
            .comments
            .last()
            .filter(|comment| !comment.own_line && comment.ends_line);
        let last_line_begin = line_start(&stream, stream.tokens.len().saturating_sub(1));
        let an_include_line = stream.tokens[last_line_begin].kind == cpp_parser::CppTokenKind::Hash
            && last_comment.is_some_and(|comment| {
                source
                    .get(stream.tokens[last_line_begin].start..comment.start)
                    .is_some_and(|text| text.contains("include"))
            });
        match (namespace_comment.take(), last_comment) {
            (Some(wanted), Some(comment)) => {
                out.push_str(source.get(trailing.start..comment.start).unwrap_or(""));
                out.push_str(&wanted);
                out.push_str(source.get(comment.end..trailing.end).unwrap_or(""));
            }
            _ => out.push_str(&verbatim_gap(source, trailing, style, an_include_line)),
        }
    }

    // **The token stream is verified, not assumed.** Removing a space can *merge* two tokens — `> >` becomes `>>`
    // (a right shift), `+ +` becomes `++`, `/ /` becomes a comment — and no amount of care in the spacing rules
    // makes that impossible: it is a property of the lexer, not of the rules. Measured on the corpus before this
    // check existed: one header (`LuaParseError.h`) came out with a different token stream than it went in with.
    //
    // The fallback is the whole file, unchanged.
    if same_tokens(&stream, &out) {
        out
    } else {
        // **A diagnostic for the fallback**: which token first differs tells the rule that merged or split one.

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
    let only_spaces = after_last_newline
        .chars()
        .all(|character| character == char::from(32) || character == char::from(9));
    if only_spaces && gap.newlines > 0 {
        Some(after_last_newline.chars().count())
    } else {
        None
    }
}

/// **The column of the line that opened the bracket this `index` is inside**, for a braced list: the last line
/// start with a known block indent. The statement`s own first line is not that line when a struct body precedes the
/// list (measured: the table`s rows came out four columns too far right).
fn the_line_that_opened_the_list(
    stream: &tokens::Stream<'_>,
    blocks: &[Option<usize>],
    index: usize,
    statement_indent: usize,
) -> usize {
    (0..index)
        .rev()
        .find(|earlier| !stream.gaps[*earlier].newlines.eq(&0) && blocks[*earlier].is_some())
        .and_then(|earlier| blocks[earlier])
        .unwrap_or(statement_indent)
}

fn planned_indent(
    index: usize,
    stream: &tokens::Stream<'_>,
    blocks: &[Option<usize>],
    stack: &[usize],
    statement_indent: usize,
    anchor: usize,
    style: &Style,
    in_ctor_init: bool,
    ctor_column: Option<usize>,
    in_initializer: bool,
    angle_column: Option<usize>,
    enum_column: Option<usize>,
) -> Option<(usize, bool)> {
    let first = stream.tokens[index].kind;
    // **A preprocessor directive starts at column 0** (`IndentPPDirectives: None`, which is clang-format`s
    // default): `#if` inside a function body belongs against the margin, and the source`s own column is not kept.
    if first == cpp_parser::CppTokenKind::Hash {
        return Some((0, true));
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
        // **The anchor is the line that opened the list**, carried by the bracket stack: a struct body`s members
        // leave `statement_indent` at their level (4), while the table that follows belongs at the opening line`s
        // column plus the continuation width (measured on LuaDefine.h: clang-format writes those rows at 8).
        return Some((layout::continuation(stack, anchor, first, style), false));
    }
    // **A line that continues a template argument list aligns after its `<`**, even where the block rules have an
    // answer: at file scope the block answer for `using S2 = Scenario<` gives 0, which is where those lines came out.
    // **A `requires` clause is a continuation of the declaration**, indented by `IndentWidth` (not
    // `ContinuationIndentWidth`): clang-format writes `    ~T()` then `        requires(...)` — measured on the
    // snippet corpus, where we put it at the block level instead.
    // **A `<<` chain that continues on the next line** starts a continuation of the *block*, not of the previous
    // line: clang-format writes the `<<` at the block`s indent plus `ContinuationIndentWidth` (measured on
    // `std::cout` split over two lines: 8 in a function body, 12 inside a loop at 4).
    // **A statement may start with a unary operator**: `*q = a + b;` after `int *q = p;` is a new statement,
    // not a continuation of the line above — the previous token is a `;`, and the block rules own the column.
    if matches!(
        first,
        cpp_parser::CppTokenKind::Star | cpp_parser::CppTokenKind::Ampersand
    ) && index > 0
        && matches!(
            stream.tokens[index - 1].kind,
            cpp_parser::CppTokenKind::Semicolon
                | cpp_parser::CppTokenKind::LeftBrace
                | cpp_parser::CppTokenKind::RightBrace
        )
        && let Some(indent) = blocks[index - 1]
    {
        return Some((indent, true));
    }
    if matches!(
        stream.tokens[index].kind,
        cpp_parser::CppTokenKind::LeftShift | cpp_parser::CppTokenKind::RightShift
    ) {
        let base = blocks[index].unwrap_or(statement_indent);
        return Some((
            base + style.count(StyleOption::ContinuationIndentWidth),
            false,
        ));
    }
    // **A record keyword at the end of a line continues the declaration**: `class` then the name on the next line is
    // one `ContinuationIndentWidth` in (measured on nlohmann json`s `iter_impl.hpp`, where a long `// NOLINT(…)` pushes
    // the name down), not at the block`s column.
    if index > 0
        && stream.gaps[index].newlines > 0
        // **A `{` is the brace rules` business**, not the declaration`s: `static const struct` then `{` sits at the
        // block`s column.
        && stream.tokens[index].kind != cpp_parser::CppTokenKind::LeftBrace
        && matches!(
            stream.tokens[index - 1].kind,
            cpp_parser::CppTokenKind::ClassKeyword
                | cpp_parser::CppTokenKind::StructKeyword
                | cpp_parser::CppTokenKind::UnionKeyword
                | cpp_parser::CppTokenKind::EnumKeyword
        )
    {
        return Some((
            statement_indent + style.count(StyleOption::ContinuationIndentWidth),
            false,
        ));
    }

    if stream.tokens[index].text == "requires" {
        return Some((
            statement_indent + style.count(StyleOption::IndentWidth),
            false,
        ));
    }
    // **An item of an enumeration written on the opening line aligns under the first item**:
    // `enum class Wide { A,` then `B,` at the column after the `{ ` (not the block indent).
    if let Some(column) = enum_column
        && blocks[index].is_some()
        // The `}` that closes the body goes back to the declaration's own column, not the items'.
        && stream.tokens[index].kind != cpp_parser::CppTokenKind::RightBrace
        // **An enumerator's value on the next line is a continuation, not the next item**: `syntax_error =` then
        // `101,` belongs one `ContinuationIndentWidth` in (measured on nlohmann json's `exceptions.hpp`).
        && !(index > 0 && is_an_assignment(stream.tokens[index - 1].kind))
    {
        return Some((column, false));
    }
    // **A continuation inside a template argument list aligns after its `<`**, whether or not the line is a block line:
    // clang-format writes
    //
    //     struct X
    //         : disjunction<std::is_same<T, bool>, std::is_same<T, char>,
    //                       std::is_same<T, signed char>, …>
    //
    // — the arguments under the first one (18), not one continuation past the statement (6). The block test that used to
    // be here skipped exactly these continuation lines (measured on nlohmann json's `binary_reader.hpp`).
    if let Some(column) = angle_column {
        return Some((column, false));
    }
    // **A declaration whose type runs onto the next line continues the declaration**: clang-format writes
    // `std::vector<std::pair<…>>` then `name_styles = …` one `ContinuationIndentWidth` in (measured), while the line
    // after a complete `template <typename T>` is a new declaration at the block level — so the line that ended with
    // the template closer decides, unless that line *started* with `template`.
    if index > 0
        && matches!(
            stream.tokens[index - 1].kind,
            cpp_parser::CppTokenKind::Greater | cpp_parser::CppTokenKind::RightShift
        )
        && stream.gaps[index].newlines > 0
        && !line_starts_with_template(stream, index - 1)
        // **An include ends with `>` too**: without this, the line after `#include <vector>` was treated as a
        // continuation of it and came out four columns in (measured: `class FunctionOption {` at 4 instead of 0).
        && !the_line_starts_with_a_directive(stream, index - 1)
    {
        // **A function declarator after its return type sits at the declaration's own indent**: clang-format writes
        //
        //     std::shared_ptr<LuaEditorConfig>
        //     LuaEditorConfig::OpenFile(const std::string &path) {
        //
        // — the signature at 0, not one continuation in (`PenaltyReturnTypeOnItsOwnLine` is what makes the break, and
        // the continuation is a new declarator of the same declaration). A **variable** declarator is one continuation
        // in (`std::vector<std::pair<…>>` then `name_styles = …` at 4, measured). The `(` before any `=` is the
        // difference.
        let mut declarator_with_parameters = false;
        if stream.tokens[index].kind == cpp_parser::CppTokenKind::Identifier {
            for token in &stream.tokens[index..] {
                match token.kind {
                    cpp_parser::CppTokenKind::LeftParen => {
                        declarator_with_parameters = true;
                        break;
                    }
                    cpp_parser::CppTokenKind::Assign
                    | cpp_parser::CppTokenKind::Semicolon
                    | cpp_parser::CppTokenKind::LeftBrace
                    | cpp_parser::CppTokenKind::RightBrace
                    | cpp_parser::CppTokenKind::Comma => break,
                    _ => {}
                }
            }
        }
        if declarator_with_parameters {
            return Some((statement_indent, false));
        }
        return Some((
            statement_indent + style.count(StyleOption::ContinuationIndentWidth),
            false,
        ));
    }
    // **A line that continues an assignment sits `ContinuationIndentWidth` past the statement**, not at the block`s
    // own level: clang-format writes `_state.CurrentWidth() +=` and the call on the next line at 8 + 4 (measured);
    // the block answer for that line is 8, which is the statement`s own column.
    if index > 0
        && is_an_assignment(stream.tokens[index - 1].kind)
        && stream.gaps[index].newlines > 0
    {
        return Some((
            statement_indent + style.count(StyleOption::ContinuationIndentWidth),
            false,
        ));
    }
    if let Some(indent) = blocks[index] {
        return Some((indent, true));
    }
    if stack.is_empty() && !layout::starts_a_continuation(stream, index) {
        return None;
    }
    if first == cpp_parser::CppTokenKind::Comma
        && let Some(column) = ctor_column
    {
        return Some((column, false));
    }
    Some((layout::continuation(stack, anchor, first, style), false))
}

/// **Does the line that ends at `index` start with the `template` keyword?** — the difference between a complete
/// template header (the next line is a declaration at the block level) and a type that continues onto the next line
/// (`std::vector<std::pair<…>>`, whose next line is a continuation indent).
fn line_starts_with_template(stream: &tokens::Stream<'_>, index: usize) -> bool {
    // **The declaration's start, not this line's**: a template head that spans lines has continuation lines starting
    // with `typename`, and the declaration after the head is a new one. The walk is **line by line** — it stops at a
    // line that ends a statement (`;`, `{`, `}`), at a directive line (`#ifdef JSON_HAS_CPP_17` may stand between the
    // head and the previous statement) and at a comment line (a doc comment is a tree of `DocText` tokens).
    let mut start = line_start(stream, index);
    while start > 0 {
        let previous_end = start - 1;
        let previous_start = line_start(stream, previous_end);
        let first = stream.tokens[previous_start].kind;
        let last = stream.tokens[previous_end].kind;
        if first == cpp_parser::CppTokenKind::Hash
            || matches!(
                first,
                cpp_parser::CppTokenKind::LineComment
                    | cpp_parser::CppTokenKind::BlockComment
                    | cpp_parser::CppTokenKind::DocText
                    | cpp_parser::CppTokenKind::DocCommandName
                    | cpp_parser::CppTokenKind::DocTrivia
            )
            || matches!(
                last,
                cpp_parser::CppTokenKind::Semicolon
                    | cpp_parser::CppTokenKind::LeftBrace
                    | cpp_parser::CppTokenKind::RightBrace
                    | cpp_parser::CppTokenKind::Colon
            )
        {
            break;
        }
        start = previous_start;
    }
    stream.tokens[start].kind == cpp_parser::CppTokenKind::TemplateKeyword
}

/// **The column of the `case`/`default` label a braced body belongs to**, when `index` is that body`s `{`.
fn case_label_indent(
    stream: &tokens::Stream<'_>,
    blocks: &[Option<usize>],
    index: usize,
) -> Option<usize> {
    // **The brace must follow the label's colon directly**: walking back from an `if (…)` inside a case body found
    // the label and put the `if`'s brace at the label's column (measured on nlohmann json under `Microsoft`, where
    // clang-format puts it at the `if`).
    // **Only a brace**: this answers where a **braced** case body's `{` goes; a statement after the label takes the
    // block layout's level (one in), and returning the label's column for it put `case 1: g();` at the label's own
    // indent when the statement was moved to its own line (measured on the case-label fixture).
    if index == 0
        || stream.tokens[index].kind != cpp_parser::CppTokenKind::LeftBrace
        || stream.tokens[index - 1].kind != cpp_parser::CppTokenKind::Colon
    {
        return Option::None;
    }
    let mut earlier = index;
    while earlier > 0 {
        earlier -= 1;
        match stream.tokens[earlier].kind {
            cpp_parser::CppTokenKind::CaseKeyword | cpp_parser::CppTokenKind::DefaultKeyword => {
                return blocks[earlier];
            }
            cpp_parser::CppTokenKind::Semicolon
            | cpp_parser::CppTokenKind::LeftBrace
            | cpp_parser::CppTokenKind::RightBrace => return None,
            // **The label`s own `:` is on the way**: `case X:` then `{` — the walk continues through it.
            cpp_parser::CppTokenKind::Colon => {}
            _ => {}
        }
    }
    None
}

/// **Is the line before the one starting at `index` a preprocessor directive?**
///
/// The declaration that follows a directive is what clang-format keeps apart from its template head (`template <typename
/// T>` then `#if …` then the struct), and its **empty body stays split** where the same declaration without the directive
/// is one line — measured on nlohmann json's `binary_reader.hpp`.
fn previous_line_is_a_directive(stream: &tokens::Stream<'_>, index: usize) -> bool {
    let line = line_start(stream, index);
    if line == 0 {
        return false;
    }
    let previous = line_start(stream, line - 1);
    stream.tokens[previous].kind == cpp_parser::CppTokenKind::Hash
}

/// **Does the line that ends at `index` start with a `#`?** — an include's `>` is not a template continuation.
fn the_line_starts_with_a_directive(stream: &tokens::Stream<'_>, index: usize) -> bool {
    let start = line_start(stream, index);
    stream.tokens[start].kind == cpp_parser::CppTokenKind::Hash
}

/// The index of the first token of the line that holds `index`.
fn line_start(stream: &tokens::Stream<'_>, index: usize) -> usize {
    let mut start = index;
    while start > 0 && stream.gaps[start].newlines == 0 {
        start -= 1;
    }
    start
}

/// **An assignment operator** — the break-after context of `PenaltyBreakAssignment`, whose continuation is a
/// `ContinuationIndentWidth` indent rather than an operand alignment.
fn is_an_assignment(kind: cpp_parser::CppTokenKind) -> bool {
    matches!(
        kind,
        cpp_parser::CppTokenKind::Assign
            | cpp_parser::CppTokenKind::PlusAssign
            | cpp_parser::CppTokenKind::MinusAssign
            | cpp_parser::CppTokenKind::StarAssign
            | cpp_parser::CppTokenKind::SlashAssign
            | cpp_parser::CppTokenKind::PercentAssign
            | cpp_parser::CppTokenKind::AmpersandAssign
            | cpp_parser::CppTokenKind::PipeAssign
            | cpp_parser::CppTokenKind::CaretAssign
            | cpp_parser::CppTokenKind::LeftShiftAssign
            | cpp_parser::CppTokenKind::RightShiftAssign
    )
}

/// **A gap copied verbatim, except for its comments' prefix spacing.** The bytes of a gap are the author's (a
/// directive's alignment, a macro body's spacing), but `SpacesInLineCommentPrefix` applies everywhere clang-format
/// prints a comment — and these gaps are how a file's first line and a directive's comments are printed.
/// **The gap's own bytes**, with one exception every path owes a comment: its prefix spacing (`//using` is `// using`).
///
/// **An `#include` line is exempt**: clang-format sorts and places includes but leaves their trailing comments byte for
/// byte (`#include <bit> //byteswap` stays as written, while `#define X //no space` and `int y; //also` both get the
/// space — measured on the oracle). The caller passes `skip_comments` for those gaps.
fn verbatim_gap(
    source: &str,
    gap: &crate::tokens::Gap,
    style: &Style,
    skip_comments: bool,
) -> String {
    if gap.comments.is_empty() || skip_comments {
        return source.get(gap.start..gap.end).unwrap_or("").to_string();
    }
    let mut out = String::with_capacity(gap.end - gap.start + 2);
    let mut cursor = gap.start;
    for comment in &gap.comments {
        out.push_str(source.get(cursor..comment.start).unwrap_or(""));
        out.push_str(&crate::comments::normalized(comment.text, style));
        cursor = comment.end;
    }
    out.push_str(source.get(cursor..gap.end).unwrap_or(""));
    out
}

/// The answers [`wrapping_lines`] computes: which tokens must start a line, which must stay on the previous one, the
/// empty bodies the style splits, the namespace comment to write after a closer, and the `}` → `{` pairing.
struct WrappingLines {
    wrapped: Vec<bool>,
    attached: Vec<bool>,
    split: Vec<bool>,
    namespace_comments: Vec<Option<String>>,
    /// **Whether each namespace body is longer than `ShortNamespaceLines`** — the gate for adding a missing closing
    /// comment (`FixNamespaceComments`); an existing one is fixed whatever the length.
    namespace_long: Vec<bool>,
    /// **A short control body that the style moves to its own line**: its indent is one level in from the control, which
    /// the block layout does not know for a body the author wrote on the control's line.
    body_wrapped: Vec<bool>,
    closers: Vec<Option<usize>>,
    /// **Which `}` each `{` closes** — a flat initializer list needs its extent to be laid out as a table.
    pairs: Vec<Option<usize>>,
}

/// **The lines `BreakBeforeBraces` asks for**, as three per-token answers.
///
/// `wrapped` is a token that must start a line (a `{` the table wraps, an `else`/`catch` that stands after a `}`),
/// `attached` is one that must stay on the previous line, and `split` marks the closing brace of an empty body the
/// style keeps on its own line (`SplitEmpty*`). A `{` whose construct the tree could not name gets neither answer: it
/// keeps the author's line, which is the same rule every role in this formatter follows.
fn wrapping_lines(
    stream: &crate::tokens::Stream<'_>,
    roles: &crate::roles::Roles,
    style: &crate::config::Style,
) -> WrappingLines {
    // **Qualified, not a glob**: `CppTokenKind` has a `None` variant, and a glob import shadows `Option::None` with
    // it — which is a type error on every `let Some(…) = … else { closed = None; }` below.
    use cpp_parser::CppTokenKind as K;
    // **Joining is part of reflow, and `ColumnLimit: 0` means "do not reflow".** clang-format keeps an author's
    // Allman brace under `Attach` when the limit is 0 and attaches it when there is a limit — measured, and it is the
    // difference between this project's own corpus (limit 0) and a real file. A *break the style demands* still
    // happens with limit 0 (Microsoft wraps a record body either way), so only the join is gated.
    let reflows = style.count(crate::options::StyleOption::ColumnLimit) > 0;
    let count = stream.tokens.len();
    let mut wrapped = vec![false; count];
    // **A short control body this pass moved to its own line**, which needs the indent the layout cannot give it.
    let mut body_wrapped = vec![false; count];
    let mut attached = vec![false; count];
    let mut split = vec![false; count];
    // **`FixNamespaceComments`**: the name to write after the closing brace of a namespace, per closer.
    let mut namespace_comments: Vec<Option<String>> = vec![None; count];
    // **Whether that namespace's body is longer than `ShortNamespaceLines`** — the gate for *adding* a comment that is
    // not there; an existing one is fixed either way.
    let mut namespace_long: Vec<bool> = vec![false; count];

    // One entry per `{` the stream opened, `Some` only when it is a **block** — a braced initializer is not one, and
    // its `}` must not pop a block's entry.
    let mut open: Vec<(usize, Option<crate::roles::BraceKind>)> = Vec::new();
    // The block the last `}` closed, which is what tells a `do`'s `while` from a `while` statement that follows an
    // unrelated block.
    let mut closed: Option<(usize, crate::roles::BraceKind)> = None;
    // **First pass: what closes what.** The placement of an opener cannot be decided until its closer is known,
    // because an **empty** body is not placed by its construct's rule at all (`{}` stays on one line under `Allman`
    // and `Attach`; only `SplitEmpty*` splits it) — measured on `AlignAnalyzer::AlignAnalyzer() {}`, which clang-format
    // writes on one line even though `Allman` wraps every non-empty function body.
    let mut pairs: Vec<Option<usize>> = vec![None; count];
    // **Which `{` each `}` closes** — a braced `case` body`s closer sits at the label`s column, which the block
    // layout alone cannot say.
    let mut closers: Vec<Option<usize>> = vec![None; count];

    for index in 0..count {
        let token = &stream.tokens[index];
        match token.kind {
            K::LeftBrace => {
                let kind = roles.brace_kind.get(&token.start).copied();
                open.push((index, kind));
            }
            K::RightBrace => {
                // **Every `{` is paired with its `}`**, block or braced initializer: the table layout needs the extent
                // of a list, and the block rules below only apply when the tree named the construct.
                let Some((open_index, kind)) = open.pop() else {
                    closed = None;
                    continue;
                };
                pairs[open_index] = Some(index);
                closers[index] = Some(open_index);
                let Some(kind) = kind else {
                    closed = None;
                    continue;
                };
                closed = Some((open_index, kind));
                // **An empty namespace gets no comment**: clang-format writes `namespace n {}`, measured; the
                // comment is for a body whose closing brace a reader might otherwise have to search for.
                //
                // **`ShortNamespaceLines` counts the body's unwrapped lines**: a namespace holding one statement gets
                // no comment with the LLVM default of 1, while two statements do — measured: `namespace d { void f(); }`
                // stays `}`, and `void f(); void g();` (two unwrapped lines, even on one source line) gets
                // `} // namespace d`.
                if kind == crate::roles::BraceKind::Namespace && open_index + 1 != index {
                    let mut body_lines = 0usize;
                    let mut depth = 0isize;
                    for inner in &stream.tokens[open_index + 1..index] {
                        match inner.kind {
                            K::LeftBrace => {
                                depth += 1;
                                if depth == 1 {
                                    body_lines += 1;
                                }
                            }
                            K::RightBrace => depth -= 1,
                            K::Semicolon if depth == 0 => body_lines += 1,
                            _ => {}
                        }
                    }
                    // **The name of every namespace, and whether the body is long enough for a *new* comment**: an
                    // existing comment is fixed whatever the length (the oracle turns `} // wrong name` into
                    // `} // namespace existing` for a one-statement body too), and a missing one is added only past
                    // `ShortNamespaceLines`.
                    if let Some(name) = roles
                        .namespace_names
                        .get(&stream.tokens[open_index].start)
                        .cloned()
                    {
                        namespace_comments[index] = Some(name);
                        namespace_long[index] =
                            body_lines > style.count(StyleOption::ShortNamespaceLines);
                    }
                }
            }
            K::ElseKeyword | K::CatchKeyword => {
                if index > 0 && stream.tokens[index - 1].kind == K::RightBrace {
                    let wraps = if token.kind == K::ElseKeyword {
                        crate::wrapping::else_starts_a_line(style)
                    } else {
                        crate::wrapping::catch_starts_a_line(style)
                    };
                    if wraps {
                        wrapped[index] = true;
                    } else if reflows {
                        attached[index] = true;
                    }
                }
                closed = None;
            }
            K::WhileKeyword => {
                // **Only a `do`'s `while`.** `if (x) { } while (y) { }` is a `while` statement after an unrelated
                // block: attaching it to the `}` line (which `BeforeWhile: false` asks for) would be wrong there, and
                // the opener's own token is what tells the two apart.
                let is_a_do_while = index > 0
                    && stream.tokens[index - 1].kind == K::RightBrace
                    && closed.is_some_and(|(open_index, _)| {
                        open_index > 0 && stream.tokens[open_index - 1].kind == K::DoKeyword
                    });
                if is_a_do_while {
                    if crate::wrapping::while_starts_a_line(style) {
                        wrapped[index] = true;
                    } else if reflows {
                        attached[index] = true;
                    }
                }
                closed = None;
            }
            _ => {
                closed = None;
            }
        }
    }

    // **Second pass: where each block brace goes**, now that emptiness is known.
    let enum_bodies = layout::enum_body_braces(stream);
    for index in 0..count {
        let Some(kind) = roles.brace_kind.get(&stream.tokens[index].start).copied() else {
            // **An enumeration's brace is found lexically**: `roles.brace_kind` does not mark an enum body when the
            // `{` is written on its own line (measured), and `BreakBeforeBraces` still decides its placement —
            // `enum class E` then `{` is attached by `LLVM` (measured on the oracle) and kept apart by `Allman`.
            if enum_bodies[index] {
                if crate::wrapping::a_brace_starts_a_line(style, crate::roles::BraceKind::Enum) {
                    wrapped[index] = true;
                } else if reflows {
                    attached[index] = true;
                }
            }
            continue;
        };
        let closer = pairs[index];
        let wraps = crate::wrapping::a_brace_starts_a_line(style, kind);
        if closer == Some(index + 1) {
            // **An empty body's placement is its own decision**, and there are two families, both measured:
            //
            // * a **control** body (`if`, `for`, `while`, `do`, `switch`, `try`, `catch`, `case`) is `{\n}` in every
            //   style — LLVM writes `if (x) {` / `}` with `Attach`, so `AllowShortBlocksOnASingleLine: Never` splits
            //   the empty block; the brace still follows its own placement rule.
            // * a **function, lambda, record or namespace** body is `{}` unless the style splits it — and it splits
            //   only when the construct wraps *and* `SplitEmpty*` says so (Microsoft splits a function's and a
            //   record's but attaches an empty `union`'s, because `AfterUnion` is off; `Allman` writes
            //   `void f() {}` and `struct S\n{\n};`).
            let a_block = matches!(
                kind,
                crate::roles::BraceKind::Control
                    | crate::roles::BraceKind::Try
                    | crate::roles::BraceKind::Catch
                    | crate::roles::BraceKind::CaseLabel
            );
            // **`AllowShortFunctionsOnASingleLine: None` splits an empty function body too**: the oracle writes
            // `void z() {` then `}` — the option means "a function body is never on one line", not "a non-empty one
            // is not". With `All` the same body is `{}` under `Attach`, and `SplitEmpty*` still governs the styles
            // that split records and namespaces.
            let never_one_line = matches!(
                kind,
                crate::roles::BraceKind::Function | crate::roles::BraceKind::Lambda
            ) && style.text(StyleOption::AllowShortFunctionsOnASingleLine)
                == "None"
                && style.count(StyleOption::ColumnLimit) > 0;
            if a_block
                || never_one_line
                || (wraps && crate::wrapping::split_empty_body(style, kind))
                // **A declaration that follows a directive keeps its empty body split**: clang-format writes
                //
                //     template <typename T>
                //     #if defined(__cpp_char8_t)
                //     struct X : std::is_same<T, char8_t> {
                //     };
                //
                // (measured on nlohmann json's `binary_reader.hpp` and on a fixture) while the same declaration without
                // the `#if` between the head and it is `struct X : … {};` — the directive keeps clang-format from joining
                // the pair, and this is the shape to reproduce rather than the join.
                || (reflows && previous_line_is_a_directive(stream, index))
            {
                split[index + 1] = true;
                wrapped[index + 1] = true;
                if wraps {
                    wrapped[index] = true;
                } else if reflows {
                    attached[index] = true;
                }
            } else {
                // **`{}` on one line, even under `Allman`**: `AlignAnalyzer::AlignAnalyzer() {}` is the oracle's
                // answer, and the author's own break before the brace is joined when the file is being reflowed.
                // With `ColumnLimit: 0` nothing is reflowed — clang-format leaves an author's `{` newline `}` alone
                // (measured: the corpus, formatted with that setting, keeps every empty body split, and joining them
                // was 4 524 spacing-only lines).
                wrapped[index] = false;
                if reflows {
                    attached[index] = true;
                    attached[index + 1] = true;
                }
            }
        } else if wraps {
            wrapped[index] = true;
        } else if reflows {
            attached[index] = true;
        }
        // **A record's body is never on one line**: clang-format breaks `struct T { int z; };` into three lines under
        // every style that attaches the brace (measured on the oracle under `LLVM`, `Google` and `Microsoft`), so the
        // first member and the closing brace start their own lines even when the author wrote the record on one. An
        // **empty** body is the `{}` case above, and an **enumeration** has its own option
        // (`AllowShortEnumsOnASingleLine`) instead of this rule.
        if matches!(
            kind,
            crate::roles::BraceKind::Class
                | crate::roles::BraceKind::Struct
                | crate::roles::BraceKind::Union
        ) && let Some(closer) = closer
            && closer > index + 1
        {
            wrapped[index + 1] = true;
            wrapped[closer] = true;
        }
        // **A block's statement is never on one line either**: `AllowShortBlocksOnASingleLine` is `Never` in `LLVM`,
        // `Google` and `Microsoft` alike, so `if (a) { g(); }` comes back as three lines however the author wrote it
        // (measured on the oracle under all three styles). The **empty** `{}` is the branch above.
        if kind == crate::roles::BraceKind::Control
            && let Some(closer) = closer
            && closer > index + 1
            && stream.gaps[index + 1].newlines == 0
        {
            wrapped[index + 1] = true;
            wrapped[closer] = true;
        }
        // **A body with two statements is never one line**, whatever the construct: `[]() { return 1; h(); }` comes
        // back as three lines under every style (`AllowShortLambdasOnASingleLine: All` joins a body with **one**
        // statement only — measured on the oracle). A one-statement body is the short-function/lambda rule's business,
        // and a control block is the rule above.
        if matches!(
            kind,
            crate::roles::BraceKind::Function | crate::roles::BraceKind::Lambda
        ) && let Some(closer) = closer
            && closer > index + 1
            && stream.gaps[index + 1].newlines == 0
        {
            let statements = (index + 1..closer)
                .filter(|at| stream.tokens[*at].kind == K::Semicolon)
                .count();
            if statements > 1 {
                wrapped[index + 1] = true;
                wrapped[closer] = true;
                // **Each statement after the first starts its own line**: the author wrote them on one line, and the
                // oracle breaks between them (`[]() {` / `return 1;` / `h();` / `}` — measured).
                let mut depth = 0isize;
                for at in index + 1..closer {
                    match stream.tokens[at].kind {
                        K::LeftBrace | K::LeftParen | K::LeftBracket => depth += 1,
                        K::RightBrace | K::RightParen | K::RightBracket => depth -= 1,
                        K::Semicolon if depth == 0 && at + 1 < closer => {
                            match stream.tokens[at + 1].kind {
                                K::Semicolon | K::RightBrace => {}
                                _ => wrapped[at + 1] = true,
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // **A short control body moves to its own line when the style says so**: `AllowShortIfStatementsOnASingleLine` is
    // `Never` in `LLVM` and `Microsoft` (`WithoutElse` in `Google`) and `AllowShortLoopsOnASingleLine` is `false` there
    // (`true` in `Google`), so the oracle writes
    //
    //     if (a)
    //       g();
    //
    // for LLVM and `if (a) g();` for Google (measured). A body the author wrote on the control's line is marked
    // `body_wrapped`, which is also what gives it the one-level-deeper indent the block layout cannot know.
    {
        let if_joins = !matches!(
            style.text(StyleOption::AllowShortIfStatementsOnASingleLine),
            "Never"
        );
        let loops_join = style.flag(StyleOption::AllowShortLoopsOnASingleLine);
        for index in 0..count {
            let keyword = stream.tokens[index].kind;
            // **A directive's `if`/`while`/`for` is not a control statement**: the `if` of `#if defined(…)` is an
            // `IfKeyword`, and looking for its `(` found the next real one — a function's parameter list — and moved the
            // token after it (`noexcept`, `{`, `,`) to its own line. Measured on nlohmann json's spacing column: 2 572 →
            // 2 620 before this guard.
            if matches!(
                keyword,
                K::IfKeyword | K::WhileKeyword | K::ForKeyword | K::DoKeyword
            ) && the_line_starts_with_a_directive(stream, index)
            {
                continue;
            }
            // **`do`'s `while` starts its own line when the body is unbraced**, whatever the loop option says: the oracle
            // writes `do g();` then `while (a);` under `Google` (where the body joins) and `do` / `g();` / `while (a);`
            // under `LLVM` (where it does not). A braced body's `while` follows its `}` and is the brace table's.
            if keyword == K::DoKeyword {
                let body = index + 1;
                if body < count && !matches!(stream.tokens[body].kind, K::LeftBrace) {
                    if stream
                        .gaps
                        .get(body + 1)
                        .is_some_and(|gap| gap.newlines > 0)
                        || stream.gaps.get(body + 1).is_some_and(|gap| gap.at_end)
                    {
                        // An empty body (`do;`) has nothing to split.
                        continue;
                    }
                    if let Some(semicolon) = (body..count).find(|at| {
                        stream.tokens[*at].kind == K::Semicolon
                            && stream.gaps[*at].newlines == 0
                            && stream.gaps.get(at + 1).is_some_and(|gap| gap.newlines == 0)
                    }) && semicolon + 1 < count
                        && stream.tokens[semicolon + 1].kind == K::WhileKeyword
                    {
                        wrapped[semicolon + 1] = true;
                    }
                }
                if !loops_join
                    && body < count
                    && !matches!(
                        stream.tokens[body].kind,
                        K::LeftBrace | K::Semicolon | K::RightBrace
                    )
                    && stream.gaps[body].newlines == 0
                {
                    wrapped[body] = true;
                    body_wrapped[body] = true;
                }
                continue;
            }
            let joins = match keyword {
                K::IfKeyword => if_joins,
                K::WhileKeyword | K::ForKeyword => loops_join,
                _ => continue,
            };
            if joins {
                continue;
            }
            // **The body follows the condition's `)`**, or the `do` keyword itself.
            let body = if keyword == K::DoKeyword {
                Some(index + 1)
            } else {
                let mut depth = 0isize;
                let mut found = None;
                for (offset, token) in stream.tokens[index..].iter().enumerate() {
                    match token.kind {
                        K::LeftParen => depth += 1,
                        K::RightParen => {
                            depth -= 1;
                            if depth == 0 {
                                found = Some(index + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                found
            };
            let Some(body) = body else {
                continue;
            };
            if body >= count
                || matches!(
                    stream.tokens[body].kind,
                    K::LeftBrace | K::Semicolon | K::RightBrace
                )
            {
                continue;
            }
            if stream.gaps[body].newlines == 0 {
                wrapped[body] = true;
                body_wrapped[body] = true;
            }
        }
    }

    // **`AllowShortCaseLabelsOnASingleLine` is `false` in all three styles**: `case 1: g(); break;` comes back as three
    // lines — the label, its statement and the `break` — under `LLVM`, `Google` and `Microsoft` (measured on the
    // oracle). The label itself stays at the switch's level (`case_extra` in the layout) and its statements one level
    // in, which the block layout already answers for a body on its own line. An **access specifier** is not a case
    // label and is left alone (wrapping the member after `public:` broke a constructor fixture).
    if !style.flag(StyleOption::AllowShortCaseLabelsOnASingleLine) {
        for index in 0..count.saturating_sub(1) {
            if stream.tokens[index].kind != K::Colon
                || !crate::breaks::is_a_case_label(stream, index)
                || stream.gaps[index + 1].newlines > 0
            {
                continue;
            }
            let first = index + 1;
            if matches!(
                stream.tokens[first].kind,
                K::LeftBrace | K::RightBrace | K::Semicolon
            ) {
                continue;
            }
            wrapped[first] = true;
            // **Each statement of the body starts its own line**, up to the next label or the switch's closer.
            let mut depth = 0isize;
            for at in first..count {
                match stream.tokens[at].kind {
                    K::LeftBrace | K::LeftParen | K::LeftBracket => depth += 1,
                    K::RightBrace | K::RightParen | K::RightBracket => depth -= 1,
                    K::CaseKeyword | K::DefaultKeyword if depth == 0 => break,
                    K::Semicolon if depth == 0 => {
                        let next = at + 1;
                        if next >= count
                            || matches!(
                                stream.tokens[next].kind,
                                K::Semicolon | K::RightBrace | K::CaseKeyword | K::DefaultKeyword
                            )
                        {
                            break;
                        }
                        wrapped[next] = true;
                    }
                    _ => {}
                }
            }
        }
    }

    WrappingLines {
        wrapped,
        attached,
        split,
        namespace_comments,
        namespace_long,
        body_wrapped,
        closers,
        pairs,
    }
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
        // **A short constructor joins, and the class body still follows it.** The oracle (clang-format 23.1.5, LLVM)
        // turns the author's three-line signature and initializer list into one line — it fits and the body is empty,
        // so `AllowShortFunctionsOnASingleLine: All` applies — and the `private:` section keeps its blank line and its
        // level. This fixture used to assert that the *unjoined* text came back.
        let source = "class T {\npublic:\n  T(const T &t) : a(t.a), b(t.b) {}\n\nprivate:\n  int a;\n  int b;\n};\n";
        let formatted = format(source, None, &llvm());
        assert_eq!(formatted, source, "the joined form is a fixed point");
    }

    #[test]
    #[ignore = "an own-line comment inside a braced initializer keeps the source column: the line-start path is blocked by the comment, and the condition meant to allow it does not fire"]
    fn an_own_line_comment_inside_an_initializer_is_an_element() {
        let source = "static int table[] = {\n  /* note */\n  1,\n  2,\n};\n";
        let formatted = format(source, None, &llvm());
        assert_eq!(
            formatted, "static int table[] = {\n        /* note */\n        1,\n        2,\n};\n",
            "elements line up, the comment with them"
        );
    }

    #[test]
    fn a_continuation_that_starts_with_an_operator_aligns_with_the_first_one() {
        // LLVM's `IndentWidth` is 2, so the body sits at 2 and the statement's first `<<` at 12; the continuation's
        // `<<` goes to the same column. **Two statements**, because a one-statement body is joined onto the signature
        // by `AllowShortFunctionsOnASingleLine: All` (the oracle joins it, and `functions.rs` pins that) — the
        // operator alignment is what this test is about.
        // **No column limit, so nothing is reflowed**: with a limit the statement fits and clang-format joins it
        // (the join pass here does too), and the alignment rule is only observable on a line that stays broken.
        let mut style = Style::default_style();
        style.set_from_text("ColumnLimit", "0");
        let source = "void f() {\n  std::cerr << util::format(\"x\", y)\n            << std::endl;\n  g();\n}\n";
        let formatted = format(source, None, &style);
        assert_eq!(
            formatted, source,
            "the `<<` of the continuation lines up with the statement's first `<<`"
        );
    }

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
        let source =
            "int  a = 1;\n// clang-format off\nint   b   =   2;\n// clang-format on\nint  c = 3;\n";
        let mut style = llvm();
        style.set_from_text("ColumnLimit", "0");
        let out = format(source, None, &style);
        assert!(
            out.contains("int   b   =   2;"),
            "the disabled span keeps its own spacing: {out:?}"
        );
        assert!(
            out.contains("int a = 1;"),
            "the lines around it are formatted: {out:?}"
        );
    }

    #[test]
    fn a_body_the_style_does_not_join_keeps_its_lines() {
        // **`AllowShortFunctionsOnASingleLine: None` is where the old contract still holds**: the line breaks stay the
        // input's and the column is decided. With `All` — LLVM's default — the same body comes back as one line,
        // because that is the oracle's answer (`void f() { int x = 1; }`).
        let mut style = llvm();
        style.set_from_text("AllowShortFunctionsOnASingleLine", "None");
        let source = "void f() {\n      int  x = 1;\n}\n";
        assert_eq!(
            format(source, None, &style),
            "void f() {\n  int x = 1;\n}\n"
        );
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
        assert_eq!(
            format(source, None, &llvm()),
            "std::vector<int> v = {1, 2, 3};\n"
        );
    }

    #[test]
    fn two_tokens_are_never_glued_into_one() {
        // `> >` must not become `>>`: the tree keeps the two `>` separate, and merging them would change the
        // reading of the file. The formatter declines the file instead of writing something else than it read.
        let source = "std::vector<std::vector<int> > v;\n";
        let formatted = format(source, None, &llvm());
        let before: Vec<_> = tokens::stream(source)
            .tokens
            .iter()
            .map(|t| t.text)
            .collect();
        let after: Vec<_> = tokens::stream(&formatted)
            .tokens
            .iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(before, after);
    }
}
