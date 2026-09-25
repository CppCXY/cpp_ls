//! Statements.
//!
//! Statements and declarations are the same construct in C++ (a declaration *is* a statement), and
//! telling them apart is the parser's central ambiguity. [`parse_declaration_or_expression_statement`]
//! is where that is resolved; the control-flow rules below are ordinary recursive descent.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MacroEvidence, MarkerEventContainer},
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
        // …and this brace is a *block*, whatever class body encloses it: a `:` in here is never a bit-field's
        // width. See `CppParser::is_at_class_member_level`.
        p.enter_block_body();
        parse_stats(p);
        p.leave_block_body();
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
    // **How many `}` this block owes to statements it gave up on** — the statement-level half of the brace debt
    // the class body keeps (`grammar/cpp/decls.rs`, and `docs/grammar-gaps.md` B58). A statement that failed after
    // consuming a `{` — an initialiser, a lambda's body, a nested block — leaves the block one brace short, and the
    // recovery below skips *to* the next `}`, which belongs to the failed statement: the block ends there and every
    // statement after it is left to the enclosing rule.
    //
    // ```cpp
    // void f() {
    //   S s{
    // #if X
    //     1
    // #endif
    //     ;                 // the `;` is in the other branch: invalid, and the member fails
    //   };
    //   after();            // ← this used to be outside `f`
    // }
    // ```
    let mut unclosed_braces = 0isize;

    while !block_follow(p) || unclosed_braces > 0 {
        if p.is_eof() {
            break;
        }

        // A `}` with a debt outstanding closes the statement that was abandoned, not this block.
        if p.current_token() == CppTokenKind::RightBrace {
            unclosed_braces -= 1;
            let error = p.mark(CppSyntaxKind::ErrorNode);
            p.bump();
            error.complete(p);
            continue;
        }

        let level = p.open_marks();
        let before_events = p.current_event_count();
        match parse_stat(p) {
            Ok(_) => {}
            Err(err) => {
                p.push_error(err);

                // `?` early returns inside `parse_stat` leave markers open. Close them before
                // recovering, otherwise the event stream stays unbalanced and corrupts the rest
                // of the file rather than just this statement.
                p.recover_to_level(level);

                // Charge the failed statement for the braces it consumed and never closed — from the events,
                // because a rule that rolled back (the declaration/expression choice does) has already truncated
                // them, and what it read is then re-read below as rubble.
                unclosed_braces += p.brace_balance_since(before_events).max(0);

                // Skip to next semicolon or closing brace for error recovery…
                let before = p.current_token_index();
                while !p.is_eof()
                    && p.current_token() != CppTokenKind::Semicolon
                    && p.current_token() != CppTokenKind::RightBrace
                {
                    if p.current_token() == CppTokenKind::LeftBrace {
                        unclosed_braces += 1;
                    }
                    p.bump();
                }
                if p.current_token() == CppTokenKind::Semicolon {
                    p.bump();
                }

                // …**and carry on with the block**, unless the recovery consumed nothing — the one case that
                // would spin this loop, and the one where the token is the `}` this block ends at.
                //
                // The `break` that used to stand here is what made a single unreadable statement cost the whole
                // block: everything after it was left to the enclosing rule, whose recovery is coarser still. It
                // is the shape `bits/stl_map.h` pays for — `__glibcxx_function_requires(…)` is a macro written
                // without its `;`, so `operator[]` failed, `iterator __i = lower_bound(__k);` was swallowed by
                // the skip, and the block ended at the next `if` with "expected `}`". `std::map`'s member list
                // stopped at line 511 and `m.find` answered "not declared in this file".
                if p.current_token_index() == before {
                    // A `{` that nothing claimed is the one token worth reading before giving up: it opens
                    // something the statement never finished, so the block owes a `}` for it.
                    if p.current_token() == CppTokenKind::LeftBrace {
                        unclosed_braces += 1;
                        let error = p.mark(CppSyntaxKind::ErrorNode);
                        p.bump();
                        error.complete(p);
                        continue;
                    }
                    break;
                }
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

        // `__try` / `__catch(…)` — the implementation's spellings of the same statement. See
        // [`at_the_implementations_try`] for why the spelling is the right thing to match on here rather than a
        // table lookup, and for the measurements behind it.
        CppTokenKind::Identifier if at_the_implementations_try(p) => parse_try_statement(p),

        // An **`asm` statement** — the compilers' own statement, whose payload is not C++ at all. Claimed here,
        // before the name is read as an expression, because `asm` is an ordinary identifier to this lexer and the
        // declaration/expression rule has nothing to make of the operands. See [`at_an_asm_statement`].
        CppTokenKind::Identifier if at_an_asm_statement(p) => parse_asm_statement(p),
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

        // A **macro invocation used as a statement**: `BOOL_OPTION(tab_width)`, `IF_EXIST(k) { … }`.
        //
        // Claimed here, before the declaration/expression question is even asked, and only for a name this file
        // **`#define`s** — evidence rather than a convention. See [`at_a_macro_call_statement`].
        //
        // **A macro that is a declaration head comes first** (B90): `STDMETHOD(QueryInterface) (…) PURE;` is also
        // "a name this file defines, invoked", and claiming it as a whole statement is what takes the rest of the
        // line away from the declaration reading. The body is what tells the two apart — see
        // [`a_macro_head_with_a_parameter_list`].
        _ if a_macro_head_with_a_parameter_list(p) => parse_a_declaration_head_macro(p),

        _ if at_a_macro_call_statement(p) => parse_macro_call(p),

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

        // A macro from a header standing where a declaration goes, which is how every libstdc++ header opens a
        // namespace version: `_GLIBCXX_BEGIN_NAMESPACE_VERSION` on a line of its own. See the rule.
        CppTokenKind::Identifier if at_a_macro_that_stands_for_a_declaration(p) => {
            parse_a_macro_that_stands_for_a_declaration(p)
        }

        // Everything else is a declaration or an expression statement.
        _ => parse_declaration_or_expression_statement(p),
    };

    if result.is_err() {
        p.close_marks_above(base);
    }

    result
}

/// Does an **`asm` statement** start at the cursor?
///
/// The compilers' own statement, and the one whose payload this grammar must not try to read:
///
/// ```cpp
/// __asm__ volatile ("tilerelease" ::);                        // amxtileintrin.h:56
/// __asm__ __volatile__ ("pconfig\n\t" : "=a" (retval) : "a" (leaf) : "cc");
/// __asm__ ("int {$}3" : );                                    // _mingw.h:584
/// ```
///
/// `asm` is not a C++ keyword — it is C's, and an extension spelled the same way by GCC and by MSVC — so this
/// lexer produces an ordinary identifier and the *shape* has to claim it: one of the three spellings, an
/// optional `volatile`/`inline`/`goto` (and GCC's `__volatile__`, which is a plain name to the lexer), and then a
/// **balanced group** — `( … )` for GCC, `{ … }` for MSVC's `__asm { … }`.
///
/// That shape has no competitor at statement position: a name followed by a parenthesised group is otherwise a
/// *call*, and a call is read — the difference is the spelling, and the three spellings are the compilers' own.
/// The one thing that outranks a spelling here is **evidence**: a file that `#define`s `asm` (or a caller whose
/// table describes it) is asking for the macro rules instead, and it gets them.
fn at_an_asm_statement(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier
        || !matches!(p.current_token_text(), "asm" | "__asm" | "__asm__")
        || p.macro_evidence(p.current_token_text()).is_some()
    {
        return false;
    }

    let mut offset = 1usize;
    loop {
        match p.peek_token_kind_at(offset..offset + 1).first() {
            Some(
                &CppTokenKind::VolatileKeyword
                | &CppTokenKind::InlineKeyword
                | &CppTokenKind::GotoKeyword,
            ) => offset += 1,
            Some(&CppTokenKind::Identifier)
                if matches!(p.peek_token_text_at(offset), "__volatile__" | "__volatile") =>
            {
                offset += 1;
            }
            Some(&CppTokenKind::LeftParen | &CppTokenKind::LeftBrace) => return true,
            _ => return false,
        }
    }
}

/// Read an `asm` statement: its keyword, its qualifiers, and its payload **as tokens**.
///
/// The payload is the compiler's operand language, not C++ — `"int {$}3":`, `[ret] "=r" (ret)`, `"a" (leaf)` —
/// so it is kept as the balanced group it is, with the tokens in order inside [`CppSyntaxKind::AsmStat`]. Reading
/// it as anything else would mean inventing a grammar for a language this layer does not implement, and would
/// lose the text a consumer wants to show.
///
/// The `;` is required after a `( … )` payload and **not** after a `{ … }` one: MSVC's `__asm { … }` is a block
/// and ends without a semicolon, which is a difference in the language rather than in this rule's convenience.
fn parse_asm_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AsmStat);

    p.bump(); // the keyword

    loop {
        match p.current_token() {
            CppTokenKind::VolatileKeyword
            | CppTokenKind::InlineKeyword
            | CppTokenKind::GotoKeyword => p.bump(),
            CppTokenKind::Identifier
                if matches!(p.current_token_text(), "__volatile__" | "__volatile") =>
            {
                p.bump();
            }
            _ => break,
        }
    }

    let (open, close, closes_itself) = match p.current_token() {
        CppTokenKind::LeftParen => (CppTokenKind::LeftParen, CppTokenKind::RightParen, false),
        CppTokenKind::LeftBrace => (CppTokenKind::LeftBrace, CppTokenKind::RightBrace, true),
        _ => {
            p.close_marks_above(base);
            return Err(CppParseError::syntax_error_from(
                "expected `(` after `asm`",
                p.current_token_range(),
            ));
        }
    };

    // The balanced group, token by token. The lexer has already made strings and comments single tokens, so a
    // bracket inside one cannot be mistaken for the payload's own.
    let mut depth = 0isize;
    while !p.is_eof() {
        let kind = p.current_token();
        if kind == open {
            depth += 1;
        } else if kind == close {
            depth -= 1;
            if depth == 0 {
                p.bump();
                break;
            }
        }
        p.bump();
    }

    if depth != 0 {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "unterminated `asm` payload",
            p.current_token_range(),
        ));
    }

    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
    } else if !closes_itself {
        p.emit_missing_node();
        p.push_error(CppParseError::syntax_error_from(
            "expected `;`",
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

/// Does a **macro invocation used as a statement** start at the cursor?///
/// The shape is a name, a parenthesised group, and then whatever the macro's body supplies — nothing at all for
/// `#define NUMBER_OPTION(op) if (…) { … }`, a block for `#define IF_EXIST(op) if (…)`, a `;` for an ordinary
/// function-like macro. What makes the reading available is that the name is one this file **`#define`s**, and
/// that is the whole point of [`crate::parser::MacroNames`]:
///
/// ```text
/// #define BOOL_OPTION(op) … ;   BOOL_OPTION(flag)      a macro whose body is a whole statement — read as one
///                                g(flag)               a call with its `;` missing — still an error
/// ```
///
/// The two are the same tokens up to the name. A *spelling* convention (all caps) would accept both, which is why
/// it is **not** used here: taking this reading means accepting a statement that is not valid C++ unless the macro
/// supplies the rest, so it needs evidence rather than a guess. The convention stays where it was — as the
/// fallback for a macro from a header, in [`super::decls::a_macro_definition_follows`], where the alternative is a
/// syntax error either way.
///
/// # The second form: a macro from a header, where the tokens *end* the statement
///
/// One family of macros from an included header is used this way and *only* this way — the concept-requirement
/// macros libstdc++ defines as nothing at all:
///
/// ```cpp
/// __glibcxx_function_requires(_LessThanComparableConcept<_Tp>)     // bits/stl_algobase.h:237
/// //return __b < __a ? __b : __a;
/// if (__b < __a)                                                   // the next token cannot continue a call
///   return __b;
/// ```
///
/// and the same shape at the end of an `#if` branch, where the next token is the directive:
///
/// ```cpp
/// #if __cplusplus < 201103L
///   __glibcxx_function_requires(_SGIAssignableConcept<_Tp>)        // bits/move.h:233
/// #endif
/// ```
///
/// Neither name is in any table this parser is handed, and both readings of those tokens are real, so that rule is
/// drawn as narrowly as the shapes allow — see [`at_a_macro_call_statement_without_evidence`], which is where it
/// lives and where its three boundaries are written down.
fn at_a_macro_call_statement(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::LeftParen
        && p.macro_evidence(p.current_token_text())
            .is_some_and(MacroEvidence::may_be_a_statement_without_a_semicolon)
}

/// Is there a **macro invocation from an included header** at the token `index`?
///
/// The form [`at_a_macro_call_statement`] cannot claim, because no table knows the name. Three boundaries, each
/// one bought by something going wrong without it:
///
/// * the name must be one the **implementation reserved** — it starts with an underscore
///   ([`super::types::written_in_the_implementations_namespace`]). A name like `FOO` is *also* how a macro is
///   written, but it is how a user's function is written too, and a file-local macro of their own would be
///   `#define`d in this file — which is evidence, and this rule has none. `FOO(x)` with no `;` keeps its error
///   (`a_macro_from_a_header_can_stand_where_a_declaration_goes` says so);
/// * what follows the group must be a token that **cannot continue the expression** — see [`ends_a_statement`]. A
///   block is deliberately **not** one of them, because `g(x) { }` is the mistake the block form of this rule
///   already has to weigh (see [`super::decls::a_macro_definition_follows`]);
/// * the reading is asked for **after** the declaration reading. At file scope `MACRO(args) name (…)` is a
///   *declaration whose specifiers are the macro* (`docs/grammar-gaps.md` B73), and a rule that claimed it first
///   would take that reading away — the first version of this one did exactly that, and
///   `a_macro_may_stand_between_the_type_and_the_declarator` caught it.
///
/// The declaration-or-expression rule asks this about the token the statement *began* at, because the declaration
/// reading may have consumed the whole shape and reported nothing: `__glibcxx_function_requires(_Concept<T>)` is a
/// perfectly good **function declaration** of that name with one unnamed parameter, and what tells the two apart is
/// that a declaration ends at its `;` — the macro's body supplies that `;`, so there is none, and the token after
/// the group cannot continue a declaration.
/// Does a macro invocation stand here **where a declaration head goes**, with the file's own parameter list after it?
///
/// `commdlg.h:577` writes, inside the block that `DECLARE_INTERFACE_(IPrintDialogCallback,IUnknown) {` opened:
///
/// ```cpp
///     STDMETHOD(QueryInterface) (THIS_ REFIID riid,LPVOID *ppvObj) PURE;
/// ```
///
/// and `combaseapi.h` — in the branch the condition layer puts in force for a C++ compilation — says
/// `STDMETHOD(method)` is `virtual COM_DECLSPEC_NOTHROW HRESULT STDMETHODCALLTYPE method`: a declaration head that
/// **ends at a name**, and that name is the parameter the file's own argument (`QueryInterface`) replaces. So the
/// head is not a call: a body that is a declaration head cannot be the callee of one, and the declarator the file
/// wrote — the parameter list — is what follows.
///
/// Asked of the **body** rather than of the spelling, because this file's `MacroNames` never saw the definition
/// (it is in an included header) and the shape alone is a call. Four conditions, and each one is what keeps a
/// mistake out:
///
/// * the body ends at an identifier — the hole the argument fills;
/// * the body holds a specifier only a **declaration** has (`virtual`, `typedef`, `class`, `struct`, `union`,
///   `inline`, `static`, `extern`): `#define MAX(a, b) ((a) > (b) ? (a) : (b))` ends at `)` and is not claimed;
/// * the invocation is followed by `(`, which is the declarator the file wrote for the name the macro holds;
/// * and **that group is not the end of the statement** — the predicate added after the first attempt was measured
///   wrong without it. `DECLARE_HANDLE(CO_MTA_USAGE_COOKIE);` and `__glibcxx_numbers(_Float16, F16);` are also
///   "a body that ends at a name, invoked", and claiming them took the corpus from 435 clean to 432 (and left
///   `numbers` a *worse* file than before). A declaration head is followed by the **declarator**, so a `(` must
///   come after the invocation's own group.
pub(super) fn a_macro_head_with_a_parameter_list(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier || p.peek_next_token() != CppTokenKind::LeftParen {
        return false;
    }

    let index = p.current_token_index();
    if kind_after_the_balanced_group(p, index) != Some(CppTokenKind::LeftParen) {
        return false;
    }

    let offset = p.current_token_range().start_offset;
    let Some(kinds) = p.macro_body_kinds_at(p.current_token_text(), offset) else {
        return false;
    };

    if kinds.last() != Some(&CppTokenKind::Identifier) {
        return false;
    }

    kinds.iter().any(|kind| {
        matches!(
            kind,
            CppTokenKind::VirtualKeyword
                | CppTokenKind::TypedefKeyword
                | CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::InlineKeyword
                | CppTokenKind::StaticKeyword
                | CppTokenKind::ExternKeyword
        )
    })
}

/// Read a macro invocation that **is** a declaration's head: `NAME ( arguments ) ( parameters ) [ MACRO ] ;`.
///
/// The declarator's **name** is in the macro's arguments and is not a token of this file — the one thing this
/// reading cannot put in the tree, and the reason the head stays a `MacroCall` rather than being dressed up as a
/// specifier sequence. Everything the file *did* write is read as what it is: the arguments are the macro's, the
/// parameter list is the declarator's, and the `;` ends the declaration.
fn parse_a_declaration_head_macro(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declaration);

    let call = p.mark(CppSyntaxKind::MacroCall);
    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);
    if p.current_token() == CppTokenKind::LeftParen {
        super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)?;
    }
    call.complete(p);

    // The declarator the file wrote. Its `(…)` is a **parameter list** and not a call's arguments, which is the
    // whole difference this reading makes.
    super::decls::parse_parameter_list(p)?;

    // What stands between the parameter list and the `;`: `PURE` is `= 0` in the same header the head came from,
    // and it is read as a macro here for the same reason the head is — the body is what says so.
    while p.current_token() == CppTokenKind::Identifier
        && p.macro_body_kinds_at(p.current_token_text(), p.current_token_range().start_offset).is_some()
    {
        let call = p.mark(CppSyntaxKind::MacroCall);
        let name = p.mark(CppSyntaxKind::NameExpr);
        p.bump();
        name.complete(p);
        call.complete(p);
    }

    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
    } else {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `;` after a declaration a macro heads",
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

/// Read the invocation at `start` as a statement.
///
/// Two shapes, and the difference is not cosmetic. A macro whose body is a statement or a whole definition
/// (`TEST(A, B) { … }`) can be followed by a block and a `;`, which [`parse_macro_call`] reads. One whose body
/// is a **brace** — `namespace __8 {`, `}` — is the whole of what the file wrote: no group, no block, no `;`,
/// and the group-reading rule refuses a bare name outright (B87).
fn parse_a_macro_invocation_statement(p: &mut CppParser, start: usize) -> ParseResult {
    if body_shapes_the_braces(p, start) {
        return parse_a_macro_that_stands_for_a_declaration(p);
    }

    parse_macro_call(p)
}

/// Does the macro written at `index` have a body — in this file — that opens a namespace or closes a brace?
///
/// `bits/c++config.h` writes `inline _GLIBCXX_BEGIN_NAMESPACE_VERSION` and, twenty lines later,
/// `_GLIBCXX_END_NAMESPACE_VERSION`, and the two macros are `namespace __8 {` and `}` **in that same file**:
/// nothing among the file's own tokens says a namespace opened or a brace closed, so the declarations that
/// followed were read as the continuation of a declaration that never ends — ``expected `;` `` at the `inline`,
/// and then at every `#if` boundary down the file. The question is asked of the macro's own `#define` body,
/// which the directive rule records as token kinds ([`CppParser::record_macro_body`]) — no include, no index,
/// no expansion pass, and the answer is about *this* file's text.
///
/// The two shapes are the whole of it. A body that opens a namespace is a namespace definition whose head the
/// file wrote as an invocation, and a body that is `}` closes the innermost brace — so the invocation is the
/// whole of the statement, and what the macro stands for is decided by the body rather than guessed by shape.
/// `namespace` must be the **first** token: `_GLIBCXX_MATH_NS` is `__8` (a namespace *name*, not a head) and
/// belongs to the declarator rules, not this one.
fn body_shapes_the_braces(p: &CppParser, index: usize) -> bool {
    if p.token_kind_at(index) != CppTokenKind::Identifier {
        return false;
    }

    p.macro_body_kinds(p.token_text_at(index)).is_some_and(|kinds| {
        kinds.first() == Some(&CppTokenKind::NamespaceKeyword)
            || kinds.as_slice() == [CppTokenKind::RightBrace]
    })
}

pub(super) fn a_macro_invocation_starts_at(p: &CppParser, index: usize) -> bool {
    if body_shapes_the_braces(p, index) {
        return true;
    }

    let name = p.token_text_at(index);
    p.token_kind_at(index) == CppTokenKind::Identifier
        && p.token_kind_at(super::decls::next_significant_index(p, index)) == CppTokenKind::LeftParen
        && p.macro_evidence(name).is_none()
        && super::types::written_in_the_implementations_namespace(name)
        && kind_after_the_balanced_group(p, index).is_some_and(ends_a_statement)
}

/// The kind of the first significant token **after** the balanced group that follows the token at `index`.
///
/// `None` when there is no such group (an unbalanced one, or a `;` before it closes — the call already ended, so
/// the question does not arise).
fn kind_after_the_balanced_group(p: &CppParser, index: usize) -> Option<CppTokenKind> {
    let mut index = super::decls::next_significant_index(p, index);
    let mut depth = 0isize;
    while index < p.token_count() {
        match p.token_kind_at(index) {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    let after = super::decls::next_significant_index(p, index);
                    return Some(p.token_kind_at(after));
                }
            }
            CppTokenKind::Semicolon | CppTokenKind::Eof | CppTokenKind::None => return None,
            _ => {}
        }
        index += 1;
    }

    None
}

/// Can this token **not** continue the expression that ends with the group before it — so that the group was the
/// whole of what the file wrote there?
///
/// The list is deliberately made of answers that end a statement rather than of operators: `if`, `#`, `}`, `return`
/// and a following name all say the invocation is complete, while `.`, `[`, `(`, `->` and every operator say it
/// continues. A **block** is not in the list, because `g(x) { }` is the mistake the block form of the
/// macro-statement rule already has to weigh (see [`super::decls::a_macro_definition_follows`]).
fn ends_a_statement(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        // The scope, and the directives that end a line inside it.
        CppTokenKind::RightBrace
            | CppTokenKind::Hash
            | CppTokenKind::Eof
            | CppTokenKind::None
            // A statement keyword: what follows a complete invocation.
            | CppTokenKind::IfKeyword
            | CppTokenKind::WhileKeyword
            | CppTokenKind::ForKeyword
            | CppTokenKind::SwitchKeyword
            | CppTokenKind::DoKeyword
            | CppTokenKind::ReturnKeyword
            | CppTokenKind::TryKeyword
            | CppTokenKind::ThrowKeyword
            | CppTokenKind::BreakKeyword
            | CppTokenKind::ContinueKeyword
            | CppTokenKind::GotoKeyword
            | CppTokenKind::CaseKeyword
            | CppTokenKind::DefaultKeyword
            | CppTokenKind::ElseKeyword
            | CppTokenKind::CatchKeyword
            // …and a name, which starts the next declaration or statement rather than continuing this expression.
            // Two invocations in a row are ordinary in these headers, and the second one's follower is whatever
            // comes after both:
            //
            // ```cpp
            // __glibcxx_function_requires(_ConvertibleConcept<_ValueType1, _ValueType2>)   // :170
            // __glibcxx_function_requires(_ConvertibleConcept<_ValueType2, _ValueType1>)   // :172
            // typedef typename iterator_traits<_ForwardIterator1>::reference _ReferenceType1;
            // ```
            | CppTokenKind::Identifier
    )
        // …or the start of a **declaration**, which is the same answer one step along.
        || starts_a_new_declaration(kind)
}

/// Does a **macro invocation that stands for a declaration** start at the cursor?
///
/// `_GLIBCXX_BEGIN_NAMESPACE_VERSION` alone on a line inside a namespace body, or
/// `_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)`, is a macro from a header: `bits/c++config.h` expands the first
/// to `namespace __8 {` in one branch and to **nothing at all** in the other. No table the parser can be handed
/// knows the name — the `#define` is in an *included* file, and the external hook is not wired to a file's
/// includes (see `eat_namespace_head_macros`, and `docs/std-library.md` for why that connection is a design round
/// of its own). So the shape decides, and it decides only where it has no competitor:
///
/// ```text
/// _GLIBCXX_BEGIN_NAMESPACE_VERSION   then `template`, `class`, `typedef`, `}`   -> a macro
/// _GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)   then a declaration          -> a macro
/// x = 1;                             the `=` continues an expression        -> an assignment, unchanged
/// Widget w;                          a declarator follows                   -> a declaration, unchanged
/// FOO(x);                            the group ends at a `;`                -> the most vexing parse, unchanged
/// COUNT                              inside a body, where a missing `;` is the competing reading
/// ```
///
/// # The question is "what may follow", and it is asked of the token after the invocation
///
/// A lone name at file or namespace scope has two readings, and what tells them apart is the token after the
/// invocation — after the **group** when there is one, since the group is the macro's:
///
/// ```text
/// MACRO template<…>        a declaration can begin there   -> a macro
/// MACRO}                   the scope ends there            -> a macro
/// MACRO(A, B) int x;       a declaration can begin there   -> a macro
/// Widget w;                an identifier is not a type     -> a declaration, unchanged
/// x = 1;                   `=` continues the name          -> an assignment, unchanged
/// FOO(x);                  a `;` follows the group         -> the most vexing parse, unchanged
/// TEST(A, B) { … }         a `{` follows the group         -> a definition, unchanged
/// COUNT                    inside a body, where a missing `;` is the competing reading
/// ```
///
/// The first version of this rule asked "does a *declarator* follow the name" instead, and `x = 1;` is what that
/// cost: no declarator follows an `=`, so an assignment was read as a macro and the `=` then had no left-hand
/// side. `Widget w;` is why the answer cannot simply be "an identifier follows": that is a declaration, and it is
/// the `;`, the `{` and the identifier that say so.
///
/// # What it costs
///
/// A file that writes a macro-shaped name followed by a declaration and means something else gets a `MacroCall`
/// where a reader might have wanted an error. Nothing is invented about it, every token is still in the tree, and
/// the declarations after it are read normally — which is the whole point: not reading it cost every declaration
/// in the file (190 diagnostics in `bits/stl_algobase.h` alone).
fn at_a_macro_that_stands_for_a_declaration(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier || p.is_inside_a_body() {
        return false;
    }

    // Evidence first, and this is the documented order rather than a preference: `docs/index-design.md` fixes
    // symbol queries as **this file's table, then the caller's, then the shape**. A name the file `#define`d, or
    // one the caller's table describes, is read by the rule that knows what a macro is — the specifier sequence
    // for `MY_API Widget const w;` (where the macro is part of the *declaration*, which is a better answer than a
    // sibling `MacroCall`), the definition rule for `TEST(A, B) { }`, the statement rule for `BOOL_OPTION(x)`.
    //
    // So this rule is the complement of [`at_a_macro_member`], which *requires* evidence: that one fires where a
    // table can answer and this one where nothing can, and between them every "a macro stands here" shape has
    // exactly one owner. Firing on a name the table describes would take a reading away from the rule that has
    // evidence for it — which is what the table tests in `tests/symbols.rs` caught.
    if p.macro_evidence(p.current_token_text()).is_some() {
        return false;
    }

    let after = super::decls::kind_after_the_run_of_names(p);
    starts_a_new_declaration(after) || ends_the_scope(after)
}

/// Can a declaration begin with this token kind — by an **anchor**, or by a **type**?
///
/// The union of two lists the grammar already keeps apart: [`can_begin_a_declaration`] (the keywords that can only
/// start a declaration, plus the specifiers) and the type keywords (`int`, `void`, `auto`, …, and the elaborated
/// `class`/`struct`/`union`/`enum`). A declaration begins with one or the other, and a caller asking "may a
/// declaration begin here" needs both — which is why this is a name rather than a longer list at the call site.
///
/// What is deliberately **not** here is an ordinary identifier: `Widget w;` is a declaration in which an
/// identifier follows a name, and a rule that accepted one would read `Widget` as a macro.
fn starts_a_new_declaration(kind: CppTokenKind) -> bool {
    super::decls::can_begin_a_declaration(kind) || super::types::is_type_specifier_keyword(kind)
}

/// Does this token end the scope the cursor is in, or start a directive — so that nothing continues it?
///
/// The other half of "what may follow", beside [`starts_a_new_declaration`]: a macro standing for a declaration is
/// the last thing on its line, so `}` and a following directive are both ordinary things to find after it. `#` is
/// here rather than in the declaration list because a directive does not begin a declaration — it begins a
/// *directive*, and the declaration it is about to write is the next thing on the next line.
fn ends_the_scope(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::RightBrace | CppTokenKind::Hash | CppTokenKind::Eof | CppTokenKind::None
    )
}

/// Read a macro invocation that stands for a declaration: `NAME` or `NAME ( tokens )`.
///
/// The same node the statement form produces, for the same reason — a macro's meaning is not knowable here, and
/// dressing it up as a declaration would hide that. No `;` is consumed: a macro standing for a declaration does
/// not write one, and a `;` that is there belongs to the empty statement rule.
pub(super) fn parse_a_macro_that_stands_for_a_declaration(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::MacroCall);

    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    if p.current_token() == CppTokenKind::LeftParen {
        super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)?;
    }

    Ok(m.complete(p))
}

/// Read a macro invocation as a statement: `NAME ( tokens ) [ { … } ] [ ; ]`.
///
/// The arguments are a **balanced token group**, not an argument list of the grammar: a macro's parameters are
/// pasted into identifiers and types alike (`TEST(Format, 1k_row)`), so nothing may be interpreted. The group is
/// kept as an `ArgumentList` because that is what it is — the macro's arguments — and the block that may follow
/// belongs to the same node, because it is part of what the macro's body produced.
fn parse_macro_call(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::MacroCall);

    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)?;

    // What the macro's body supplies. A block is the body of a macro that expands to a statement or a whole
    // definition (`IF_EXIST(op) if (…)`, gtest's `TEST(A, B) { … }`); a `;` is the ordinary function-like macro;
    // and **nothing at all** is the case this rule exists for — a body that is a complete statement of its own.
    match p.current_token() {
        CppTokenKind::LeftBrace => {
            if let Err(err) = parse_compound_stat(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        CppTokenKind::Semicolon => p.bump(),
        _ => {}
    }

    Ok(m.complete(p))
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

    // `inline _GLIBCXX_BEGIN_NAMESPACE_VERSION`: the `inline` is the file's own token and everything the
    // declaration would say after it belongs to a macro whose own body is `namespace __8 {` — so the statement
    // **is** that invocation, and there is no `;` to expect because the body supplies the `{` (B87). Asked
    // before the declaration pass, which reads `inline NAME` as a declaration and then reports a missing `;`
    // at the directive that follows.
    if p.current_token() == CppTokenKind::InlineKeyword {
        let index = p.current_token_index();
        if body_shapes_the_braces(p, super::decls::next_significant_index(p, index)) {
            let m = p.mark(CppSyntaxKind::MacroCall);
            p.bump(); // `inline`, part of what the macro stands for
            let name = p.mark(CppSyntaxKind::NameExpr);
            p.bump();
            name.complete(p);
            return Ok(m.complete(p));
        }
    }

    // **A macro that is the declaration's head** (B90) — the second route to the same shape: a file that does not
    // `#define` the name itself never reaches [`parse_stat`]'s macro-statement branch, and the declaration reading
    // gets no further than the invocation's own group before it reports the `(…)` it cannot place.
    if a_macro_head_with_a_parameter_list(p) {
        return parse_a_declaration_head_macro(p);
    }

    // Anchors let the speculative declaration pass be skipped: `static`, `class`, `typename` and
    // friends can never begin an expression, so there is nothing to disambiguate.
    if super::decls::starts_declaration(p) {
        return super::decls::parse_declaration(p);
    }

    let start = p.current_token_index();
    let checkpoint = p.checkpoint();
    match super::decls::parse_declaration(p) {
        Ok(marker) => {
            // The declaration reading **succeeded**, and that is not the end of the question: a macro invocation
            // from an included header reads as a function declaration too — `__glibcxx_function_requires(_Concept<T>)`
            // is `NAME ( parameter )`, and the declaration rule has no reason to refuse it. What separates the two
            // is the `;`: a declaration has one and the macro's body supplies it, so there is none and the token
            // after the group cannot continue a declaration. Only a name the implementation reserved is asked
            // about, so `Widget w(1);` and every ordinary missing-`;` mistake keep their readings.
            if p.last_consumed_token_kind() != Some(CppTokenKind::Semicolon)
                && a_macro_invocation_starts_at(p, start)
            {
                p.rollback(checkpoint);
                return parse_a_macro_invocation_statement(p, start);
            }
            Ok(marker)
        }
        Err(_) => {
            // Not a declaration. Rewind **first**: the question below is asked about the token the statement began
            // at, and the failed attempt left the cursor somewhere past it.
            p.rollback(checkpoint);
            // The same reading is asked for once more, because a shape the declaration rule *refuses* may be this
            // one: `_foo(x)` in front of `}` reads as a declaration of a type `_foo` with a parameter list, which
            // is a declaration of nothing. See [`at_a_macro_call_statement_without_evidence`].
            if a_macro_invocation_starts_at(p, start) {
                return parse_a_macro_invocation_statement(p, start);
            }
            // Not a declaration, and not a macro from a header. Read it as an expression.
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
/// Exposed to the initializer rule, which meets directives between the elements of a table rather than between
/// statements — see [`super::decls::parse_braced_initializer`].
pub(super) fn parse_preprocessor_directive(p: &mut CppParser) -> ParseResult {
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
    // `#define` and `#undef` name a macro, and that name is what the rest of the file needs to know: a macro is
    // expanded before the grammar runs, so an invocation is recognisable only by name. See
    // [`crate::parser::MacroNames`] — and note that the *name* is all that is recorded: the replacement list is
    // read as tokens below, like the rest of the directive, and nothing tries to interpret it.
    let defines_a_macro = p.current_token() == CppTokenKind::Identifier
        && matches!(p.current_token_text(), "define" | "undef");
    let undefines_a_macro = defines_a_macro && p.current_token_text() == "undef";
    if p.current_token() == CppTokenKind::Identifier {
        p.bump();
    }

    // A `#define` body is read as token kinds too, so a later reading can ask what the macro stands
    // for without parsing its body a second time (B87). Neither the name nor a parameter list that
    // touches it names a token of the body, so neither is recorded.
    struct BodyRecording {
        name: Box<str>,
        /// The name's end offset: a `(` that touches the name is the parameter list, while
        /// `#define f (x)` is an object-like macro whose body begins with `(`.
        name_ends_at: usize,
        kinds: Vec<CppTokenKind>,
        /// 0 on the name, 1 inside the parameter list, 2 in the body.
        state: u8,
        depth: u32,
    }

    impl BodyRecording {
        fn take(&mut self, p: &CppParser) {
            let kind = p.current_token();
            match self.state {
                0 => {
                    // The name is not a token of the body; it is only what tells a parameter list from a
                    // body that begins with `(`.
                    self.state = if kind == CppTokenKind::LeftParen
                        && p.current_token_range().start_offset == self.name_ends_at
                    {
                        self.depth = 1;
                        1
                    } else {
                        2
                    };
                }
                1 => {
                    if kind == CppTokenKind::LeftParen {
                        self.depth += 1;
                    } else if kind == CppTokenKind::RightParen {
                        self.depth = self.depth.saturating_sub(1);
                        if self.depth == 0 {
                            self.state = 2;
                        }
                    }
                }
                _ => self.kinds.push(kind),
            }
        }
    }

    let mut recording = None;

    if defines_a_macro && p.current_token() == CppTokenKind::Identifier {
        let name = p.current_token_text().to_string();
        if undefines_a_macro {
            p.undefine_macro_name(&name);
        } else {
            p.declare_macro_name(&name);
            recording = Some(BodyRecording {
                name: Box::from(name.as_str()),
                name_ends_at: p.current_token_range().end_offset(),
                kinds: Vec::new(),
                state: 0,
                depth: 0,
            });
        }
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

        if let Some(recording) = recording.as_mut() {
            recording.take(p);
        }
        p.bump();
    }

    if let Some(recording) = recording {
        p.record_macro_body(&recording.name, recording.kinds);
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

    // **The tokens stay, so the node gets its end event.** `close_marks_above` would detach it instead, and an
    // unpaired `NodeStart` is balanced by the tree builder at the end of the stream — so this statement would
    // swallow everything after it, including the `}` of its own block. That is exactly what a macro written
    // without its `;` costs (`bits/stl_map.h:530`, and with it `std::map::find`): see maintenance convention 34.
    p.emit_missing_node();
    p.end_marks_to(base);
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

    // A **macro standing between `if` and its condition**: `if _GLIBCXX17_CONSTEXPR (std::is_same_v<…>)`, which is
    // how the standard library writes `if constexpr` in a header that must also compile as C++14 — there the macro
    // expands to **nothing at all**, and in C++17 to `constexpr`.
    //
    // Accepted by shape rather than by table, and the shape is decisive: `if` is followed by a `(` in every
    // program C++ accepts, so an identifier in between takes no valid program away. That is the fallback side of
    // maintenance convention 16 in `docs/grammar-gaps.md` — where a table cannot answer because the `#define` is
    // in an *included* header (`bits/c++config.h`), and where both readings of the tokens are wrong if the macro
    // is not one. It is *not* evidence-free in the way `MY_API` was: nothing legal is being re-read.
    //
    // The `(` is required, which is what keeps this away from `if consteval` — that word also arrives as an
    // identifier, and it is followed by a `{`, not by a condition.
    if p.current_token() == CppTokenKind::Identifier && p.peek_next_token() == CppTokenKind::LeftParen {
        p.bump(); // the macro that stands for `constexpr`, or for nothing
    }

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

    // The joint between an `if`'s branch and its `else` — the same seam `parse_try_statement` documents, and the
    // one the standard library hits first:
    //
    // ```cpp
    // if constexpr (__or_<…>::value)
    //   _S_copy(…);
    // #if __cpp_lib_concepts              // ← here: `bits/basic_string.h`, line 490, its first error
    // else if constexpr (requires { … })
    // ```
    //
    // Without it the `#endif`…`#if` pair is what the else-branch is read as, the `else` becomes a statement with
    // nothing before it, and the error is reported against the `}` of a class whose body is a hundred lines away.
    // Read as the node it is, then **ask again whether the token is `else`** rather than assuming: a `#` that is
    // not a directive is still an error, and this seam only accepts directives.
    if let Err(err) = eat_preprocessor_directives(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if p.current_token() == CppTokenKind::ElseKeyword {
        let else_m = p.mark(CppSyntaxKind::ElseStat);
        p.bump();

        // The other side of the same joint: `else` and the statement it introduces.
        if let Err(err) = eat_preprocessor_directives(p) {
            p.close_marks_above(base);
            return Err(err);
        }

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

    // A condition may declare a variable: `if (Foo* p = get())`, `if (const auto n = g())`.
    //
    // The declaration here is the one a **condition** reads — specifiers, one declarator, an initialiser, no `;`
    // of its own — and that is the whole of the fix, not a detail: `parse_declaration` insists on a `;`, so the
    // attempt failed, the fallback read an *expression*, and the two readings came out as a silent multiplication
    // and an error respectively:
    //
    // ```text
    // if (Foo* p = get())     read as `Foo * p = get()` — a BinaryExpr nobody reports, and not valid C++ either
    // if (Foo p = get())      `expected ), but get identifier` against the `=`
    // if (const auto n = g()) `expected primary expression` against the `=`
    // ```
    //
    // The rule's two refusals are what keep the expression path for the conditions that are expressions: a
    // condition declares **one** variable and it must be **initialised**, so `if (a && b)` and `if (v.size())`
    // never reach a declaration. See `parse_condition_declaration`.
    let checkpoint = p.checkpoint();
    if super::decls::parse_condition_declaration(p).is_err() {
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
///
/// # Attributes come first
///
/// `if (__n > 1) [[likely]]`, `else [[unlikely]]`, `while (x) [[likely]]` — C++20 lets an attribute stand on the
/// *substatement*, which is a position where nothing else may go, and the standard library's own headers use it
/// (16 occurrences in 7 files of the measured closure, every one of them after an `if` condition). Reading them
/// here rather than in each of the five statements that own a substatement is what makes one rule cover the lot:
/// the then-branch, the else-branch and every loop body reach their statement through this function, and none of
/// them can be reached any other way.
///
/// The node is a sibling of the body inside the statement, because that is what it is: an attribute *on* the
/// statement, not part of it. The reader is the same one `[[nodiscard]]` goes through — see
/// [`super::types::parse_attribute_specifiers`] — so nothing here is specific to `likely`.
fn parse_statement_body(p: &mut CppParser) -> ParseResult {
    super::types::parse_attribute_specifiers(p)?;

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

/// The name the standard library's own header gives to `try`, and to `catch`.
///
/// `bits/exception_defines.h` defines both, and its definition is the evidence for reading them as the keywords
/// they stand for:
///
/// ```text
/// #ifdef __EXCEPTIONS                     #else
/// # define __try      try                  # define __try      if (true)
/// # define __catch(X) catch(X)            # define __catch(X) if (false)
/// #endif
/// ```
///
/// # Why the spelling, and not a table
///
/// The same reasoning as the compiler's attribute spellings (`__attribute__`, `__declspec` — see
/// `docs/grammar-gaps.md`, maintenance convention 24), with the same two conditions satisfied: the names are
/// **reserved to the implementation** (a double underscore, so no conforming program may define them), and the
/// `#define` that gives them meaning is the *implementation's*, not the file's — no header a project writes can
/// turn `__try` into something else. Both matter: the rule that reads a macro from a header
/// ([`at_a_macro_that_stands_for_a_declaration`]) deliberately refuses inside a body and requires the table,
/// which is why these two spellings were not read at all.
///
/// # What the wrong reading cost, measured
///
/// `__try { g(); }` used to be an **expression**, not a statement: a name followed by `{` is list-initialisation
/// of a temporary (`Vec<int>{1, 2}`), so the block was read as an `InitListExpr` holding a statement —
/// `expected }, but get ;` against the `;` of the first statement inside it, and then the brace matching of the
/// whole function was off by one, which is what the `expected }` and the stray `ErrorNode` after it were. Of the
/// 185 files in the closure of six standard headers, **18 contain `__try`** and 17 of them fail.
///
/// The reading is a `TryStat` with `CatchStat` handlers rather than a `MacroCall`: the construct *is* the
/// statement under the branch that has exceptions on, and reading it as a macro would lose the pairing between
/// the body and its handlers — the one thing a consumer of a `try` is looking for. The `if (true)`/`if (false)`
/// branch would be a different statement, and it is the branch a reader cannot see without evaluating
/// `__EXCEPTIONS`; `docs/index-design.md` records that both branches of a conditional are read as text, and this
/// takes the reading that keeps the structure.
const THE_KEYWORD_TRY_IS_SPELLED: &str = "__try";

/// The same, for `catch` — see [`THE_KEYWORD_TRY_IS_SPELLED`].
const THE_KEYWORD_CATCH_IS_SPELLED: &str = "__catch";

/// Is the cursor on `__try`, with a block to follow?
///
/// The block is required because `__try` is *also* an MSVC keyword for structured exception handling
/// (`__try`/`__except`/`__finally`), which is a different construct with a different shape. Neither spelling
/// appears in the measured closure, so nothing is claimed about it here; requiring the `{` is what keeps a SEH
/// `__try` from being read as a `try` statement whose block happens to be there.
fn at_the_implementations_try(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Identifier
        && p.current_token_text() == THE_KEYWORD_TRY_IS_SPELLED
        && p.peek_next_token() == CppTokenKind::LeftBrace
}

/// Is the cursor on `__catch` **with its argument list**?
///
/// `__catch` is function-like by definition — its `#define` has a parameter — so the parenthesis is part of the
/// spelling, and requiring it is what tells the macro from a name that merely starts the same way. Its argument
/// list is the handler's parameter list, which is why the reading calls the same rule `catch (…)` does.
fn at_the_implementations_catch(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Identifier
        && p.current_token_text() == THE_KEYWORD_CATCH_IS_SPELLED
        && p.peek_next_token() == CppTokenKind::LeftParen
}

fn parse_try_statement(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TryStat);

    p.bump(); // Consume 'try' — or the implementation's spelling of it, which is the same one token

    // **Directives at every joint of a `try`.** A handler that exists in a debug build and not in a release one
    // is a real spelling, and the directive that decides it can land between any two tokens of the statement —
    // between `try` and its block, and between the block and the `catch`:
    //
    // ```cpp
    // #if !defined(_DEBUG)
    //     try
    // #endif
    //     {
    //         …
    //     }
    // #if !defined(_DEBUG)
    //     catch (std::exception &e) { … }
    // #endif
    // ```
    //
    // That is `IOSession.cpp` in the first real C++ project, and the shape is worth reading carefully because
    // the *first* of the two joints is the one that breaks the statement: with `try` followed by `#endif`, the
    // block was not the try's block at all, so the statement ended there and the `catch` became a statement with
    // no statement before it — reported as `expected }` against the `catch`.
    //
    // Read as the nodes they are, exactly as B23, B24 and the three other places `docs/grammar-gaps.md` records
    // for the same argument; a `#` anywhere it cannot be a directive is still an error.
    if let Err(err) = eat_preprocessor_directives(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = parse_compound_stat(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = eat_preprocessor_directives(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    while p.current_token() == CppTokenKind::CatchKeyword || at_the_implementations_catch(p) {
        let handler = p.mark(CppSyntaxKind::CatchStat);
        p.bump();

        if p.current_token() == CppTokenKind::LeftParen
            && let Err(err) = parse_parameter_list_inline(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }

        // The same joint, one handler along: `catch (E& e)` and its block.
        if let Err(err) = eat_preprocessor_directives(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        if let Err(err) = parse_compound_stat(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        handler.complete(p);

        if let Err(err) = eat_preprocessor_directives(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    Ok(m.complete(p))
}

/// Read the preprocessor directives at the cursor as nodes, and stop at the first thing that is not one.
///
/// For the constructs whose parts a conditional can separate — see [`parse_try_statement`], and B23/B24 in
/// `docs/grammar-gaps.md` for the same reading in an initializer, in a string-literal run, and between a
/// function's head and its body. The directives stay in the tree, so nothing is lost and a consumer can see
/// which branch each one guards.
fn eat_preprocessor_directives(p: &mut CppParser) -> ParseResult {
    while p.current_token() == CppTokenKind::Hash {
        parse_preprocessor_directive(p)?;
    }
    Ok(crate::parser::CompleteMarker::empty())
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
