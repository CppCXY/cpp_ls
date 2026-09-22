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

use super::{decls::parse_using_declaration, expect_token, exprs::parse_expr};

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
        // A name declared inside a brace is not a type name outside it, so the table's scope follows the
        // braces. This is the approximation the table documents: it counts *parser* depth, which is enough to
        // keep a local class from being a type name for the next function.
        p.enter_type_name_scope();
        parse_stats(p);
        p.leave_type_name_scope();

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
        CppTokenKind::IfKeyword => parse_if_statement(p),
        CppTokenKind::WhileKeyword => parse_while_statement(p),
        CppTokenKind::DoKeyword => parse_do_while_statement(p),
        CppTokenKind::ForKeyword => parse_for_statement(p),
        CppTokenKind::SwitchKeyword => parse_switch_statement(p),
        CppTokenKind::TryKeyword => parse_try_statement(p),
        CppTokenKind::ReturnKeyword => parse_return_statement(p),
        // Coroutine statements. `co_return` is `return` under another name — the same operand, the same optional
        // value, the same `;` — so it is the same rule through a different keyword rather than a copy of it.
        CppTokenKind::CoReturnKeyword => parse_return_statement(p),
        CppTokenKind::CoYieldKeyword => parse_throw_statement(p),
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
///
/// # The C++17 initializer
///
/// `if (auto q = find(x); q != nullptr)` is a *declaration* and then a *condition*, separated by a `;` — two
/// constructs where the grammar had one. The declaration is read by the for-init rule, which is the same
/// declaration without the trailing `;` that a header does not own; the `;` is then consumed here and the
/// condition follows as a plain expression.
///
/// The order matters and so does the fallback. A `;` inside the parentheses is what marks the form, and it
/// cannot be mistaken for anything else: no expression statement's `;` can appear inside a condition's
/// parentheses, because the condition's own parentheses have not closed. So the scan is a lookahead for a `;`
/// at depth zero, and when there is none the rule behaves exactly as before.
fn parse_condition(p: &mut CppParser) -> ParseResult {
    let _ = p.open_marks();
    let m = p.mark(CppSyntaxKind::ParenExpr);

    expect_token(p, CppTokenKind::LeftParen)?;

    // `if (init; condition)` — an initializer, then the condition.
    if a_semicolon_ends_an_initializer(p) {
        parse_initializer_part(p)?;
        expect_token(p, CppTokenKind::Semicolon)?;
        parse_expr(p)?;
        expect_token(p, CppTokenKind::RightParen)?;
        return Ok(m.complete(p));
    }

    // A condition may declare a variable: `if (Foo* p = get())`.
    let checkpoint = p.checkpoint();
    if super::decls::parse_declaration(p).is_err() {
        p.rollback(checkpoint);
        parse_expr(p)?;
    }

    expect_token(p, CppTokenKind::RightParen)?;
    Ok(m.complete(p))
}

/// Is there a `;` at the condition's own depth, making this an `if (init; condition)`?
///
/// A lookahead rather than a parse, because the two forms have to be told apart *before* the declaration rule
/// runs — that rule consumes a `;` as its own terminator and would take this one with it. The scan is bounded
/// and tracks nesting, so a `;` inside a lambda body or a nested parentheses is not mistaken for the
/// separator.
fn a_semicolon_ends_an_initializer(p: &CppParser) -> bool {
    let mut depth = 0isize;

    for kind in p.peek_token_kind_at(0..64) {
        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                depth += 1
            }
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            CppTokenKind::Semicolon if depth == 0 => return true,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
}

/// Parse the initializer half of `if (init; condition)`, which may be a declaration or an expression.
///
/// The same two readings a `for` header's init part has, and the same rule: the declaration is tried first
/// because it is the one that can be refused, and an expression is what is left when it is.
fn parse_initializer_part(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();

    if super::decls::parse_for_init_declaration(p).is_ok() {
        return Ok(crate::parser::CompleteMarker::empty());
    }
    p.rollback(checkpoint);

    parse_expr(p)
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

        // A range declaration is optional in C++20 (`for (auto v : m)` vs. `for (v : m)`), so a failure here
        // means the header held an expression and the range reading is still open.
        //
        // The rule is the *for-init* one rather than a whole declaration: the header has no `;` of its own, so
        // a rule expecting one reports "expected `;`" against the range expression.
        if super::decls::parse_for_init_declaration(p).is_err() {
            p.rollback(checkpoint.clone());
            parse_expr(p)?;
        }

        if p.current_token() == CppTokenKind::Colon {
            // The kind is decided *here*, before the node is closed, and set on the marker rather than on the
            // value `complete` hands back. The marker's kind is what the `NodeStart` event was emitted with, and
            // `complete` only reads it — so writing to the returned `CompleteMarker` changes a copy and leaves
            // the tree with the `ForStat` the node was opened as. That is how a range-based `for` came out
            // labelled as a C-style one while parsing perfectly.
            let mut m = m;
            m.set_kind(p, CppSyntaxKind::RangeForStat);

            p.bump();
            parse_expr(p)?;
            expect_token(p, CppTokenKind::RightParen)?;
            parse_statement_body(p)?;

            return Ok(m.complete(p));
        }

        // Not a range-for after all; fall through to the C-style reading. The checkpoint was taken before the
        // range reading, so rewinding here also undoes the optional range declaration that parsed successfully.
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
/// Called with the cursor just **inside** the opening `(`, so the header is scanned at relative depth zero and
/// a `:` at that depth is the range separator. Scanning from depth one instead — the state *before* the `(` was
/// consumed — makes every `:` look nested, so the scan never fires and `for (auto x : items)` is read as a
/// C-style header and fails on the `:`.
///
/// A `?` at depth zero rules the range reading out, because `a ? b : c` has a `:` that is not a separator.
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
                // The header's own `)` at depth zero ends it without a range separator.
                if depth < 0 {
                    return false;
                }
            }
            CppTokenKind::Question => saw_question = true,
            CppTokenKind::Semicolon if depth == 0 => return false,
            CppTokenKind::Colon if depth == 0 => return !saw_question,
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
                //
                // Read with pack expansion *refused*, because this rule owns the `...` of the range: with the
                // expansion reading on, `case 2 ... 4:` parsed as a pack expansion of `2` and then reported a
                // missing `:`. The range is the one construct in a statement where a `...` follows an
                // expression and is not an expansion.
                if let Err(err) = super::exprs::parse_expr_without_pack_expansion(p) {
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
            && let Err(err) = parse_parameter_list_inline(p)
        {
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
    if p.current_token() != CppTokenKind::Semicolon
        && !p.is_eof()
        && let Err(err) = parse_return_value(p)
    {
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
        && let Err(err) = parse_expr(p)
    {
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
