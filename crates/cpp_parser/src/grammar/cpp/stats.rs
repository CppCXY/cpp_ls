//! Statements.
//!
//! Statements and declarations are the same construct in C++ (a declaration *is* a statement), and
//! telling them apart is the parser's central ambiguity. [`parse_declaration_or_expression_statement`]
//! is where that is resolved; the control-flow rules below are ordinary recursive descent.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::{
    decls::parse_using_declaration, expect_token, exprs::parse_expr,
};

/// Parse a compound statement — `{ ... }` — or, in C++, a *single* statement.
///
/// The `{` is what distinguishes `Foo::Foo() : a(1) {}` (a function body) from
/// `Foo::Foo() : a(1);` (a declaration), so this rule drives most of the declaration/definition
/// decision and is the first thing a real declaration parser will need to hook into.
pub(crate) fn parse_compound_stat(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    if p.current_token() == CppTokenKind::LeftBrace {
        p.bump();
        parse_stats(p);

        if p.current_token() == CppTokenKind::RightBrace {
            p.bump();
            return Ok(m.complete(p));
        }

        // A missing `}` is the single most common error while editing. Keeping the block and
        // recording the absence as a zero-width node gives completion a sane place to live instead
        // of swallowing the rest of the file into an error node — but the problem must still be
        // reported, or the editor has no way to tell the user about it.
        p.emit_missing_node();
        p.push_error(CppParseError::syntax_error_from(
            "expected `}`",
            p.current_token_range(),
        ));
        return Ok(m.complete(p));
    }

    if let Err(err) = parse_stat(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse statements until a token that cannot start one.
pub fn parse_stats(p: &mut CppParser) {
    while !block_follow(p) {
        let level = p.open_marks();
        match parse_stat(p) {
            Ok(_) => {}
            Err(err) => {
                p.push_error(err);

                // `?` early returns inside `parse_stat` leave markers open. Close them before
                // recovering, otherwise the event stream stays unbalanced and corrupts the rest
                // of the file rather than just this statement.
                p.recover_to_level(level);

                // Skip to next semicolon or closing brace for error recovery
                while !p.is_eof()
                    && p.current_token() != CppTokenKind::Semicolon
                    && p.current_token() != CppTokenKind::RightBrace
                {
                    p.bump();
                }
                if p.current_token() == CppTokenKind::Semicolon {
                    p.bump();
                }
                break;
            }
        }
    }
}

fn block_follow(p: &CppParser) -> bool {
    matches!(
        p.current_token(),
        CppTokenKind::RightBrace            // }
            | CppTokenKind::Eof             // End of file
            | CppTokenKind::CaseKeyword     // case (in switch)
            | CppTokenKind::DefaultKeyword  // default (in switch)
            | CppTokenKind::ElseKeyword     // else
            | CppTokenKind::CatchKeyword // catch
    )
}

/// Dispatch to the statement rule for the current token.
///
/// Every statement passes through here, which makes this the natural enforcement point for the
/// marker contract documented in `grammar::mod`: an `Err` leaving this function has closed every
/// node the statement opened, so callers can recover by skipping tokens without having to know how
/// deep the failed rule got.
pub fn parse_stat(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();

    let result = match p.current_token() {
        // Control flow statements
        CppTokenKind::IfKeyword => parse_if_statement(p),        CppTokenKind::WhileKeyword => parse_while_statement(p),
        CppTokenKind::DoKeyword => parse_do_while_statement(p),
        CppTokenKind::ForKeyword => parse_for_statement(p),
        CppTokenKind::SwitchKeyword => parse_switch_statement(p),
        CppTokenKind::TryKeyword => parse_try_statement(p),
        CppTokenKind::ReturnKeyword => parse_return_statement(p),
        CppTokenKind::BreakKeyword => parse_keyword_statement(p, CppSyntaxKind::BreakStat),
        CppTokenKind::ContinueKeyword => parse_keyword_statement(p, CppSyntaxKind::ContinueStat),
        CppTokenKind::GotoKeyword => parse_goto_statement(p),
        CppTokenKind::ThrowKeyword => parse_throw_statement(p),

        // Compound statement
        CppTokenKind::LeftBrace => parse_compound_stat(p),

        // C++20 modules. Checked *before* the label case below: `module : private;` and
        // `module A:B;` both look exactly like `identifier :` — a label — and the label rule would
        // eat the `module` and leave the rest as a stray statement.
        //
        // This is the cost of `module` and `import` being contextual keywords rather than real ones:
        // the parser has to decide from the shape of the declaration, and it has to do so early
        // enough that no other rule claims the tokens first.
        _ if super::modules::starts_module_related_declaration(p) => {
            super::decls::parse_declaration(p)
        }

        // A label: `foo:` at the start of a statement.
        CppTokenKind::Identifier
            if p.peek_token_kind_at(1..2).as_slice() == [CppTokenKind::Colon] =>
        {
            parse_label_statement(p)
        }

        // An empty statement.
        CppTokenKind::Semicolon => {
            let m = p.mark(CppSyntaxKind::EmptyStat);
            p.bump();
            Ok(m.complete(p))
        }

        // Everything else is a declaration or an expression statement.
        _ => parse_declaration_or_expression_statement(p),
    };

    if result.is_err() {
        p.close_marks_above(base);
    }

    result
}

/// Resolve the declaration/expression ambiguity by trying the declaration reading first.
///
/// See the module documentation in `decls` for why this is decided by backtracking rather than by a
/// heuristic on the first tokens. The rewind is cheap — the parser is an event list and a token
/// cursor — and getting it wrong produces a wrong but plausible tree, which is much worse.
fn parse_declaration_or_expression_statement(p: &mut CppParser) -> ParseResult {
    // A preprocessor directive is not a C++ construct at all; it only exists at the token level.
    if p.current_token() == CppTokenKind::Hash {
        return parse_preprocessor_directive(p);
    }

    // `using` declarations cannot be expressions, so they do not need the speculative path.
    if p.current_token() == CppTokenKind::UsingKeyword {
        return parse_using_declaration(p);
    }

    // Anchors let the speculative declaration pass be skipped: `static`, `class`, `typename` and
    // friends can never begin an expression, so there is nothing to disambiguate.
    if super::decls::starts_declaration(p) {
        return super::decls::parse_declaration(p);
    }

    let checkpoint = p.checkpoint();

    match super::decls::parse_declaration(p) {
        Ok(marker) => Ok(marker),
        Err(_) => {
            // Not a declaration. Rewind and read it as an expression.
            p.rollback(checkpoint);
            parse_expression_statement(p)
        }
    }
}

/// Parse a preprocessor directive as a leaf of the syntax tree: `#` name rest-of-line.
///
/// The directive is kept as a node rather than skipped, for two reasons. It has to stay in the tree
/// for the CST to remain lossless, and the *unselected* branches of `#if` contain real
/// declarations — an editor needs to see them, both so the file parses at all and so that code
/// inside a disabled branch is not reported as garbage.
///
/// One directive gets special treatment: `#include` is followed by a header name, which the lexer
/// only produces on request because `<` and `>` are far too common to guess at.
fn parse_preprocessor_directive(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::PreprocessorDirective);

    // A directive runs to the end of its **logical** line, and a `\`-newline splice does not end one.
    //
    // The boundary has to be computed as an *offset*, not looked for as a `Newline` token: the newline
    // is trivia, and `bump` skips trivia while attaching it to the current node. So after consuming the
    // last real token of the line the cursor is already on the next line's first token, a `Newline` is
    // never seen, and a loop that waits for one consumes the whole file. That failure is not local —
    // every declaration after the first directive disappears into it.
    //
    // A splice is the other half of the same problem. `#define F(a, b) \` followed by the body on the
    // next line is one directive, and a boundary taken at the first newline byte cuts the body off
    // after the `\` — leaving a macro whose replacement list is empty while the file still round-trips,
    // which is the kind of wrong that no losslessness check can see.
    let text = p.origin_text();
    let mut line_end = logical_line_end(text, p.current_token_range().start_offset);

    p.bump(); // `#`

    // The directive name is a plain identifier (`include`, `define`, `if`, ...); the null directive
    // `#` alone on a line has none.
    let directive_is_include = p.current_token() == CppTokenKind::Identifier
        && matches!(p.current_token_text(), "include" | "include_next");
    if p.current_token() == CppTokenKind::Identifier {
        p.bump();
    }

    // Reading the rest token by token keeps the text in the tree; the preprocessor layer re-reads
    // these tokens when it needs their real meaning.
    let mut header_name_expected = directive_is_include;

    while !p.is_eof() && p.current_token_range().start_offset < line_end {
        if p.current_token() == CppTokenKind::LineContinuation {
            // The directive continues on the line after the splice, so the boundary moves with it.
            p.bump();
            line_end = logical_line_end(text, p.current_token_range().start_offset);
            continue;
        }

        if header_name_expected {
            header_name_expected = false;
            if p.try_lex_header_name() {
                continue;
            }
        }
        p.bump();
    }

    Ok(m.complete(p))
}

/// The offset at which the logical line containing `start` ends.
///
/// A newline does not end a logical line when the byte before it is a `\` — that is a splice, and
/// translation phase 2 has already removed it by the time a directive's extent matters. Only the
/// backslash immediately before the newline counts, which is what the standard says and what keeps a
/// `\\` at the end of a line from swallowing the next one.
fn logical_line_end(text: &str, start: usize) -> usize {
    let Some(rest) = text.get(start..) else {
        return text.len();
    };

    let bytes = rest.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'\n' {
            // Look back for the splice. `\r\n` is one line ending, so the check skips a `\r`.
            let before = if index > 0 && bytes[index - 1] == b'\r' {
                index.checked_sub(2)
            } else {
                index.checked_sub(1)
            };
            let spliced = before.is_some_and(|at| bytes[at] == b'\\');
            if !spliced {
                return start + index;
            }
        }
        index += 1;
    }

    text.len()
}

/// Parse an expression statement: `expr ;`.
fn parse_expression_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ExpressionStat);

    if let Err(err) = parse_expr(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // A statement ends at a `;`. Reaching something else means the expression did not cover the
    // statement, which is what makes the caller's recovery kick in.
    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
        return Ok(m.complete(p));
    }

    p.emit_missing_node();
    p.close_marks_above(base);
    Err(CppParseError::syntax_error_from(
        "expected `;` after expression",
        p.current_token_range(),
    ))
}

fn parse_if_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::IfStat);

    p.bump(); // Consume 'if'

    // `if constexpr (...)`, and C++23's `if consteval` / `if !consteval`.
    //
    // The two forms could hardly be less alike underneath. `if constexpr` still takes a parenthesised
    // condition, while `if consteval` takes *no* condition at all: the statement is selected by
    // whether the evaluation is constant, and the braces follow the keyword directly. So this is not
    // one flag but two — the keyword says which form, and only `constexpr` is followed by a condition.
    //
    // The negation is part of the second form rather than the start of an expression, which is why it
    // has to be consumed here. Leaving it to `parse_condition` reports `expected (` against a `!` that
    // is perfectly valid C++23.
    let consteval_form = if p.current_token() == CppTokenKind::ConstexprKeyword {
        p.bump();
        false
    } else if p.current_token() == CppTokenKind::ConstevalKeyword
        || (p.current_token() == CppTokenKind::Identifier && p.current_token_text() == "consteval")
    {
        p.bump();
        true
    } else if p.current_token() == CppTokenKind::LogicalNot
        && p.peek_token_kind_at(1..2).as_slice() == [CppTokenKind::ConstevalKeyword]
    {
        p.bump(); // `!`
        p.bump(); // `consteval`
        true
    } else {
        false
    };

    // `if consteval` has no condition to parse. Asking for one would report `expected (` against the
    // `{` that is really the body, which is worse than useless: it accuses correct code.
    if !consteval_form && let Err(err) = parse_condition(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = parse_statement_body(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if p.current_token() == CppTokenKind::ElseKeyword {
        let else_m = p.mark(CppSyntaxKind::ElseStat);
        p.bump();
        if let Err(err) = parse_statement_body(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        else_m.complete(p);
    }

    Ok(m.complete(p))
}

fn parse_while_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::WhileStat);

    p.bump(); // Consume 'while'

    if let Err(err) = parse_condition(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = parse_statement_body(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

fn parse_do_while_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::DoWhileStat);

    p.bump(); // Consume 'do'
    if let Err(err) = parse_statement_body(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = expect_token(p, CppTokenKind::WhileKeyword) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = parse_condition(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = expect_token(p, CppTokenKind::Semicolon) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse `( ... )` after `if`/`while`/`switch`/`for`, or a condition declaration.
fn parse_condition(p: &mut CppParser) -> ParseResult {
    let _ = p.open_marks();
    let m = p.mark(CppSyntaxKind::ParenExpr);

    expect_token(p, CppTokenKind::LeftParen)?;

    // A condition may declare a variable: `if (Foo* p = get())`.
    let checkpoint = p.checkpoint();
    if super::decls::parse_declaration(p).is_err() {
        p.rollback(checkpoint);
        parse_expr(p)?;
    }

    expect_token(p, CppTokenKind::RightParen)?;
    Ok(m.complete(p))
}

/// Parse the body of a control-flow statement: a block, or one statement.
fn parse_statement_body(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftBrace {
        return parse_compound_stat(p);
    }

    // A dangling `;` is an empty statement, which is a valid body.
    parse_stat(p)
}

fn parse_for_statement(p: &mut CppParser) -> ParseResult {
    let _ = p.open_marks();
    let m = p.mark(CppSyntaxKind::ForStat);

    p.bump(); // Consume 'for'

    // `for co_await (...)` (C++20).
    if p.current_token() == CppTokenKind::CoAwaitKeyword {
        p.bump();
    }

    expect_token(p, CppTokenKind::LeftParen)?;

    // Range-based for: `for (decl : range)`.
    if starts_a_range_for(p) {
        let checkpoint = p.checkpoint();

        // A range declaration is optional in C++20 (`for (auto v : m)` vs. `for (v : m)`).
        if super::decls::parse_declaration(p).is_err() {
            p.rollback(checkpoint);
            parse_expr(p)?;
        }

        if p.current_token() == CppTokenKind::Colon {
            p.bump();
            parse_expr(p)?;
            expect_token(p, CppTokenKind::RightParen)?;
            parse_statement_body(p)?;

            let completed = m.complete(p);
            let mut completed = completed;
            completed.kind = CppSyntaxKind::RangeForStat;
            return Ok(completed);
        }

        // Not a range-for after all; fall through to the C-style reading.
        p.rollback(checkpoint);
    }

    // C-style: `for (init ; cond ; step)`.
    if p.current_token() != CppTokenKind::Semicolon {
        parse_declaration_or_expression_statement_without_semicolon(p)?;
    }
    expect_token(p, CppTokenKind::Semicolon)?;

    if p.current_token() != CppTokenKind::Semicolon {
        parse_expr(p)?;
    }
    expect_token(p, CppTokenKind::Semicolon)?;

    if p.current_token() != CppTokenKind::RightParen {
        parse_declaration_or_expression_statement_without_semicolon(p)?;
    }
    expect_token(p, CppTokenKind::RightParen)?;

    parse_statement_body(p)?;

    Ok(m.complete(p))
}

/// Would a `:` later in the `for` header make this a range-based for?
///
/// Scans the header at nesting depth zero, where a `:` can only be the range separator. A `:` at
/// depth zero cannot appear in an ordinary for-init (`a ? b : c` is inside no parentheses but does
/// contain a `:`, so the scan stops at `?` conservatively).
fn starts_a_range_for(p: &CppParser) -> bool {
    let mut depth = 0isize;
    let mut saw_question = false;

    for kind in p.peek_token_kind_at(0..64) {
        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                depth += 1
            }
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                depth -= 1;
                if depth <= 0 {
                    return false;
                }
            }
            CppTokenKind::Question => saw_question = true,
            CppTokenKind::Semicolon if depth == 1 => return false,
            CppTokenKind::Colon if depth == 1 => return !saw_question,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
}

/// Parse the init or step part of a `for` header, which has no trailing semicolon of its own.
fn parse_declaration_or_expression_statement_without_semicolon(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();

    let checkpoint = p.checkpoint();
    if super::decls::parse_for_init_declaration(p).is_ok() {
        return Ok(crate::parser::CompleteMarker::empty());
    }
    p.rollback(checkpoint);

    let m = p.mark(CppSyntaxKind::ExpressionStat);
    if let Err(err) = parse_expr(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

fn parse_switch_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::SwitchStat);

    p.bump(); // Consume 'switch'

    if let Err(err) = parse_condition(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = expect_token(p, CppTokenKind::LeftBrace) {
        p.close_marks_above(base);
        return Err(err);
    }

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        match p.current_token() {
            CppTokenKind::CaseKeyword => {
                let case = p.mark(CppSyntaxKind::CaseStat);
                p.bump();
                // A case label may be a constant expression or a range `case 1 ... 5:`.
                if let Err(err) = parse_expr(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                if p.current_token() == CppTokenKind::Ellipsis {
                    p.bump();
                    if let Err(err) = parse_expr(p) {
                        p.close_marks_above(base);
                        return Err(err);
                    }
                }
                if let Err(err) = expect_token(p, CppTokenKind::Colon) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                case.complete(p);
            }
            CppTokenKind::DefaultKeyword => {
                let default = p.mark(CppSyntaxKind::DefaultStat);
                p.bump();
                if let Err(err) = expect_token(p, CppTokenKind::Colon) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                default.complete(p);
            }
            _ => {
                // Regular statements inside the switch body.
                parse_stat(p)?;
            }
        }
    }

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

fn parse_try_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TryStat);

    p.bump(); // Consume 'try'
    if let Err(err) = parse_compound_stat(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    while p.current_token() == CppTokenKind::CatchKeyword {
        let handler = p.mark(CppSyntaxKind::CatchStat);
        p.bump();

        if p.current_token() == CppTokenKind::LeftParen
            && let Err(err) = parse_parameter_list_inline(p) {
                p.close_marks_above(base);
                return Err(err);
            }

        if let Err(err) = parse_compound_stat(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        handler.complete(p);
    }

    Ok(m.complete(p))
}

/// Parse a catch clause's parameter list, reusing the declaration grammar.
fn parse_parameter_list_inline(p: &mut CppParser) -> ParseResult {
    super::decls::parse_parameter_list(p)
}

fn parse_return_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ReturnStat);

    p.bump(); // Consume 'return'

    // `return;` and `co_return;` are complete statements.
    if p.current_token() != CppTokenKind::Semicolon && !p.is_eof()
        && let Err(err) = parse_return_value(p) {
            p.close_marks_above(base);
            return Err(err);
        }

    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

/// Parse what follows `return`: an expression, or a braced-init-list.
fn parse_return_value(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftBrace {
        // `return {1, 2};`
        return super::decls::parse_braced_initializer(p);
    }

    parse_expr(p)
}

fn parse_keyword_statement(p: &mut CppParser, kind: CppSyntaxKind) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(kind);
    p.bump();

    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
    } else {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `;`",
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

fn parse_goto_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::GotoStat);

    p.bump(); // Consume 'goto'
    if p.current_token() == CppTokenKind::Identifier {
        p.bump();
    }
    if let Err(err) = expect_token(p, CppTokenKind::Semicolon) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

fn parse_throw_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ThrowStat);

    p.bump(); // Consume 'throw'
    if p.current_token() != CppTokenKind::Semicolon
        && let Err(err) = parse_expr(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    if let Err(err) = expect_token(p, CppTokenKind::Semicolon) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

fn parse_label_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::LabelStat);

    p.bump(); // the label
    if let Err(err) = expect_token(p, CppTokenKind::Colon) {
        p.close_marks_above(base);
        return Err(err);
    }

    // Attributes may follow a label: `foo: [[likely]]`.
    while p.current_token() == CppTokenKind::LeftBracket
        && p.peek_next_token() == CppTokenKind::LeftBracket
    {
        if super::types::parse_attribute_specifier(p).is_err() {
            break;
        }
    }

    Ok(m.complete(p))
}
