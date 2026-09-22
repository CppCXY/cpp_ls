//! Declarations.
//!
//! The grammar is `decl-specifier-seq init-declarator-list(opt) ;`, plus the function-definition
//! form where the last declarator is followed by a body instead of a semicolon. Classes, enums and
//! namespaces are declarations whose declarator is special, so they plug in here.
//!
//! # Declarations versus expressions
//!
//! This is *the* ambiguity of C++ parsing, and it cannot be resolved syntactically:
//!
//! ```text
//! a * b;      // declaration of `b` as a pointer to `a`? or multiplication?
//! T(x);       // declaration of `x`? or a functional cast / function call?
//! ```
//!
//! There is no symbol table in a parser that must work on a file being edited, so [`parse_declaration`]
//! answers the question *speculatively*: try the declaration reading, and if it does not land on a
//! plausible end (a `;`, a `{`, or an initializer), rewind and let the caller treat the tokens as an
//! expression. [`super::stats::parse_declaration_or_expression_statement`] does exactly that with
//! [`CppParser::try_parse`], which is why that primitive exists.
//!
//! The cost is one re-parse of a statement whose first token is ambiguous. The alternative — a
//! heuristic that guesses from the shape of the first two tokens — gets `T(x);` and `a * b;` wrong
//! in one direction each, and being wrong here produces a *plausible looking but incorrect tree*,
//! which is far worse for an editor than paying for a backtrack.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CompleteMarker, CppParser, Marker, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::{
    expect_token,
    exprs::parse_expr,
    stats::{parse_compound_stat, parse_stats},
    types::{
        definitely_ends_a_type, parse_decl_specifier_seq, parse_declarator, parse_declarator_with,
        parse_name, parse_type_id,
    },
};
/// Parse a template head: `template <typename T, int N>`, or the explicit-specialization
/// `template <>`.
///
/// The head is kept as its own node so a consumer can ask "is this declaration templated?" without
/// re-scanning tokens, and so the parameter list keeps its own children.
fn parse_template_head(p: &mut CppParser) -> ParseResult {
    // A template parameter list has the same `>`-closes-the-list property an argument list has:
    // `template <int N = 3>` must not read the `>` as "greater than". Restored on every exit path.
    let previous_depth = p.enter_template_arguments();
    let result = parse_template_head_inner(p);
    p.leave_template_arguments(previous_depth);
    result
}

fn parse_template_head_inner(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateDecl);

    expect_token(p, CppTokenKind::TemplateKeyword)?;

    // C++20 template parameter lists may end in a requires-clause; that comes after the list.
    expect_token(p, CppTokenKind::Less)?;

    // `template <>` — an explicit specialization has an empty list.
    if p.current_token() == CppTokenKind::Greater {
        p.bump();
    } else {
        let parameters = p.mark(CppSyntaxKind::TemplateParameterList);
        loop {
            if let Err(err) = parse_template_parameter(p) {
                p.close_marks_above(base);
                return Err(err);
            }

            match p.current_token() {
                CppTokenKind::Comma => {
                    p.bump();
                    continue;
                }
                CppTokenKind::Greater => {
                    p.bump();
                    break;
                }
                // `>>` closing a nested list, e.g. `template <template <class> class T>`.
                CppTokenKind::RightShift => {
                    p.split_current_token(1, CppTokenKind::Greater, CppTokenKind::Greater);
                    p.bump();
                    break;
                }
                _ => {
                    p.close_marks_above(base);
                    return Err(CppParseError::syntax_error_from(
                        "expected `,` or `>` in template parameter list",
                        p.current_token_range(),
                    ));
                }
            }
        }
        parameters.complete(p);
    }

    Ok(m.complete(p))
}

/// Parse one template parameter: `typename T`, `class C`, `int N`, `template <...> class T`,
/// `T...`, or a constrained parameter `C T`.
fn parse_template_parameter(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateParameter);

    // A template template parameter: `template <typename> class C`.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && let Err(err) = parse_template_head(p) {
            p.close_marks_above(base);
            return Err(err);
        }

    // The parameter itself is a declaration: `typename T`, `int N`, `auto N`, and a constrained
    // parameter is a type followed by a name. Reusing the declaration machinery keeps the shapes
    // the same as everywhere else.
    if let Err(err) = parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The element name, and a pack expansion marker.
    if !definitely_ends_a_type(p.current_token())
        && p.current_token() != CppTokenKind::Comma
        && p.current_token() != CppTokenKind::Greater
        && let Err(err) = parse_declarator(p) {
            p.close_marks_above(base);
            return Err(err);
        }

    if p.current_token() == CppTokenKind::Ellipsis {
        p.bump();
    }

    // A default argument. Which grammar applies depends on the parameter kind, and that is not
    // known here: `typename T = std::vector<int>` defaults a *type*, while `int N = 3` defaults a
    // *value*. Trying the type first and falling back to an expression resolves it without tracking
    // which kind of parameter this is — and the type reading must be tried first, because
    // `std::vector<int>` is also a perfectly good (if nonsensical) expression and reading it that
    // way would lose the template argument list.
    //
    // "Did it parse?" is not enough to decide: `parse_type_id` can succeed having consumed
    // *nothing*, which is the correct answer for a type-id that is absent. Without the progress
    // check below, `3` in `int N = 3` leaves a successful, empty type-id behind, the list loop then
    // sees `=` where it expects `,` or `>`, and the whole declaration unwinds into an error node.
    if p.current_token() == CppTokenKind::Assign {
        p.bump();

        let type_checkpoint = p.checkpoint();
        let before = p.current_token_index();
        let parsed_a_type = parse_type_id(p).is_ok() && p.current_token_index() > before;
        if !parsed_a_type {
            p.rollback(type_checkpoint);

            if let Err(err) = parse_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
    }

    Ok(m.complete(p))
}
///
/// Returns `Err` without having consumed anything when the input does not look like a declaration,
/// so the caller can fall back to parsing an expression statement.
pub fn parse_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let checkpoint = p.checkpoint();

    // Declarations whose shape is not `specifiers declarators ;` get their own rules, dispatched
    // first because the general rule would read their keyword as a type specifier and then report
    // nonsense about everything after it.
    match p.current_token() {
        CppTokenKind::NamespaceKeyword => return parse_namespace_declaration(p),
        CppTokenKind::TypedefKeyword => return parse_typedef_declaration(p),
        CppTokenKind::StaticAssertKeyword => return parse_static_assert(p),

        // `export` heads a module declaration, an export block, or one exported declaration. An
        // `export import` is a re-export, so `import` after `export` routes to the import rule.
        CppTokenKind::ExportKeyword => {
            let next = p.peek_token_kind_at(1..2)[0];
            if next == CppTokenKind::LeftBrace {
                return super::modules::parse_export_block(p);
            }
            if next == CppTokenKind::Identifier {
                // The word after `export` decides: `module` starts a module declaration, `import` a
                // re-export. Both are contextual keywords, so this is a text check.
                let second = p.peek_token_kind_at(2..3)[0];
                let _ = second;
                return super::modules::parse_exported_declaration(p);
            }
            // `export declaration` — consume the keyword and parse what it exports. Whether a
            // declaration is exported is a property the semantic layer reads off the token.
            //
            // What follows is a full declaration, and the general rule below does not cover all of
            // them: `using` and `template` are not specifiers it can start from. Dispatching them
            // here keeps `export using Point = shapes::Point;` — an everyday spelling in a module
            // interface — from being re-read as an expression and reported as broken.
            p.bump();

            match p.current_token() {
                CppTokenKind::UsingKeyword => return parse_using_declaration(p),
                CppTokenKind::NamespaceKeyword => return parse_namespace_declaration(p),
                CppTokenKind::TypedefKeyword => return parse_typedef_declaration(p),
                CppTokenKind::StaticAssertKeyword => return parse_static_assert(p),
                _ => {}
            }
        }
        _ => {}
    }

    // C++20 modules. `module` and `import` are contextual keywords, so this is a text check at the
    // start of a declaration rather than a token kind.
    if super::modules::starts_global_module_fragment(p) {
        return super::modules::parse_global_module_fragment(p);
    }
    if super::modules::starts_private_module_fragment(p) {
        return super::modules::parse_private_module_fragment(p);
    }
    if super::modules::starts_module_declaration(p) {
        return super::modules::parse_module_declaration(p);
    }
    if super::modules::starts_import_declaration(p) {
        return super::modules::parse_import_declaration(p);
    }

    let m = p.mark(CppSyntaxKind::Declaration);

    // A template head wraps whatever declaration follows it: `template <typename T> struct V {};`
    // is a template *declaration* whose payload is the class. Handling it here rather than in a
    // separate rule is what lets templates apply to classes, functions, variables, aliases and
    // concepts without five copies of the same parser.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && let Err(err) = parse_template_head(p) {
            p.rollback(checkpoint);
            return Err(err);
        }

    let specifiers_from = p.current_event_count();

    if let Err(err) = parse_decl_specifier_seq(p) {
        p.rollback(checkpoint);
        return Err(err);
    }
    // A declaration that ends right after its specifiers: `int;` (useless but legal) or, much more
    // commonly, `struct Foo;`.
    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
        return Ok(m.complete(p));
    }

    // Definitions of class-like entities: `class Foo { ... };`, `enum E { ... };`,
    // `namespace ns { ... }`. The specifier sequence already consumed the keyword and the name.
    if p.current_token() == CppTokenKind::LeftBrace && declaration_opens_a_body(p) {
        if let Err(err) = parse_class_body(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        if p.current_token() == CppTokenKind::Semicolon {
            p.bump();
        }
        return Ok(m.complete(p));
    }

    // One or more init-declarators.
    let mut declarators = 0usize;
    loop {
        if let Err(err) = parse_init_declarator(p) {
            if declarators == 0 {
                p.rollback(checkpoint);
                return Err(err);
            }
            p.close_marks_above(base);
            return Err(err);
        }
        declarators += 1;

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            continue;
        }
        break;
    }

    // A function definition: the last init-declarator was a function declarator and a body
    // follows. `parse_init_declarator` has already recorded the parameters; the body belongs to
    // the declaration.
    if p.current_token() == CppTokenKind::LeftBrace {
        if let Err(err) = parse_compound_stat(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // A class, struct, union or enum definition ends with `;` as part of the *declaration*, not as
    // an empty declaration after it. `class Foo {};` is one declaration; without this, the `;` is
    // parsed as a stray empty statement.
    if p.current_token() == CppTokenKind::Semicolon && declaration_defined_a_class(p, specifiers_from)
    {
        p.bump();
        return Ok(m.complete(p));
    }

    if let Err(err) = expect_semicolon(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Did the specifier sequence just parsed contain a class-like *definition* (a body), as opposed to
/// a forward declaration or an elaborated type specifier?
fn declaration_defined_a_class(p: &CppParser, specifiers_from: usize) -> bool {
    p.events_contain_any(
        specifiers_from,
        &[CppSyntaxKind::ClassBody, CppSyntaxKind::CompoundStat],
    )
}

/// Does the `{` at the cursor open a body that belongs to the declaration just parsed, rather than
/// an initializer or a following block?
///
/// Only a class-like or namespace specifier makes a brace a *body*; for anything else, `int x { 1 }`
/// is a brace initializer and `{ ... }` after a declaration is a new block. The distinction is
/// decided from the leading tokens of the declaration, which is enough: `struct` / `class` /
/// `union` / `enum` / `namespace` can only appear at the front.
fn declaration_opens_a_body(p: &CppParser) -> bool {
    p.peek_token_kind_at(0..4).iter().any(|kind| {
        matches!(
            kind,
            CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::EnumKeyword
                | CppTokenKind::NamespaceKeyword
        )
    })
}

/// Parse a structured binding's name list: the `[a, b]` of `auto [a, b] = pair;`.
///
/// Consumes nothing and returns `Err` when the brackets do not hold a binding pattern, which is what
/// lets a `[` that is something else be handled by its own rule.
///
/// # Why the names are `NameExpr` children
///
/// The same node a reference to a name gets, so "which names does this declare?" is one query over
/// one node kind rather than a second kind that means almost the same thing. Each name is followed by
/// an optional `...`, C++26's pack expansion, which is a token here rather than a node: nothing can
/// be said about it until the pack is known, and that is a semantic question.
pub fn parse_structured_binding(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::StructuredBinding);

    expect_token(p, CppTokenKind::LeftBracket)?;

    // An identifier or a pack expansion — `[a, b]`, `[a, ...]`, `[... xs]`. Anything else means this
    // `[` was not a binding pattern after all, and `Err` here sends it to whichever rule owns it.
    while matches!(
        p.current_token(),
        CppTokenKind::Identifier | CppTokenKind::Ellipsis
    ) {
        if p.current_token() == CppTokenKind::Ellipsis {
            p.bump();
            continue;
        }

        let name = p.mark(CppSyntaxKind::NameExpr);
        if let Err(err) = parse_name(p) {
            name.undo(p);
            p.close_marks_above(base);
            return Err(err);
        }
        name.complete(p);

        if p.current_token() != CppTokenKind::Comma {
            break;
        }
        p.bump();
    }

    if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse one `init-declarator`: a declarator plus an optional initializer or function body.
pub fn parse_init_declarator(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::InitDeclarator);
    // Where the declarator's own events start, so `finish_init_declarator` can tell whether it had a
    // parameter list.
    let declarator_from = p.current_event_count();

    // A structured binding: `auto [a, b] = pair;`, or `auto& [k, v] = map;` where the `&` comes
    // first. The plain form is handled here because it is the whole declarator; the reference form is
    // handled inside `parse_declarator`, which is the only place that knows where the name would go.
    //
    // This is not a declarator in any useful sense — no name, no derived type — so it gets its own
    // node rather than a `Declarator` holding brackets. The distinction matters to a consumer: the
    // names inside are separate variables that share one initializer, and a walk that reports "this
    // declaration declares `[a, b]`" is worse than one that reports nothing.
    if p.current_token() == CppTokenKind::LeftBracket {
        if let Err(err) = parse_structured_binding(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return finish_init_declarator(p, m, declarator_from);
    }

    // `T(x);` and `int (x);` are parenthesized declarators, not calls. Detecting that here is what
    // keeps the declarator reading from mis-nesting the parentheses.
    if p.current_token() == CppTokenKind::LeftParen {
        let checkpoint = p.checkpoint();
        let paren = p.mark(CppSyntaxKind::Declarator);
        p.bump();
        if parse_declarator(p).is_err() {
            // Not a parenthesized declarator; let the expression reading have it.
            p.rollback(checkpoint);
        } else if p.current_token() == CppTokenKind::RightParen {
            p.bump();
            paren.complete(p);
            if let Err(err) = finish_declarator_suffixes(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            return finish_init_declarator(p, m, declarator_from);
        } else {
            p.rollback(checkpoint);
        }
    }

    if let Err(err) = parse_declarator_with(p, true) {
        p.close_marks_above(base);
        return Err(err);
    }

    finish_init_declarator(p, m, declarator_from)
}

/// The part after the declarator proper: an initializer, a constructor body, or nothing.
///
/// `declarator_from` is an event index taken before the declarator was parsed, used to ask whether
/// that declarator had a parameter list. It is the only reliable way to tell a brace-or-equal
/// initializer from a function body:
///
/// ```text
/// int x{1};                 // no parameter list -> the brace is an initializer
/// int f() { again: ; }      // parameter list    -> the brace is the function's body
/// ```
///
/// Looking at the token after `{` instead does not work, and the failure is not subtle: a label
/// (`again:`) starts with an identifier, exactly like an initializer clause, so a function whose
/// first statement is a label was read as an `InitListExpr` and then reported "expected primary
/// expression" at its own first statement.
fn finish_init_declarator(p: &mut CppParser, m: Marker, declarator_from: usize) -> ParseResult {
    // A declarator with a parameter list can only declare a function, so a `{` after it opens a
    // body no matter what is inside. `parse_declaration` handles the body; this rule must not
    // consume it.
    //
    // `declarator_from` is only a fallback for callers that did not go through
    // `parse_declarator`; the flag it can be read from records that an *empty* parameter list was
    // seen, which the event stream cannot express because an empty node is dropped.
    let declarator_is_function = p.last_declarator_is_function()
        || p.events_contain_any(
            declarator_from,
            &[CppSyntaxKind::ParameterList, CppSyntaxKind::TrailingReturnType],
        );

    match p.current_token() {
        CppTokenKind::Assign => {
            p.bump();
            let init = p.mark(CppSyntaxKind::Initializer);
            if let Err(err) = parse_initializer_clause(p) {
                init.undo(p);
                return Err(err);
            }
            init.complete(p);
        }
        CppTokenKind::LeftBrace if !declarator_is_function => {
            // Brace-or-equal initializer on a variable: `Point p{1, 2};`.
            let init = p.mark(CppSyntaxKind::Initializer);
            if let Err(err) = parse_braced_initializer(p) {
                init.undo(p);
                return Err(err);
            }
            init.complete(p);
        }
        CppTokenKind::Colon => {
            // A constructor's member initializer list: `Foo() : a(1), b(2) {}`. A `:` only follows a
            // function declarator, so this is unambiguous.
            parse_member_initializer_list(p)?;
        }
        _ => {}
    }

    Ok(m.complete(p))
}

/// Continue a declarator after a parenthesized name has been consumed.
fn finish_declarator_suffixes(p: &mut CppParser) -> ParseResult {
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                parse_parameter_list(p)?;
                // `const`, `noexcept`, `override`, `-> T` and friends belong to the function
                // declarator, not to whatever comes next.
                super::types::eat_function_qualifiers(p);
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                if p.current_token() != CppTokenKind::RightBracket && !p.is_eof()
                    && let Err(err) = parse_expr(p) {
                        array.undo(p);
                        return Err(err);
                    }
                if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
                    array.undo(p);
                    return Err(err);
                }
                array.complete(p);
            }
            _ => return Ok(CompleteMarker::empty()),
        }
    }
}

/// Parse the expression or braced list on the right of `=`.
fn parse_initializer_clause(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftBrace {
        return parse_braced_initializer(p);
    }

    parse_expr(p)
}

/// Parse the declaration part of a `for` header, where the `;` belongs to the header rather than to
/// the declaration.
pub fn parse_for_init_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declaration);

    if let Err(err) = parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Err(err) = parse_init_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    while p.current_token() == CppTokenKind::Comma {
        p.bump();
        if let Err(err) = parse_init_declarator(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    Ok(m.complete(p))
}

/// Parse `{ ... }` as an initializer, keeping it as one node.
pub fn parse_braced_initializer(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::InitListExpr);

    expect_token(p, CppTokenKind::LeftBrace)?;

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        // A designated initializer: `.field = 1` (C++20) or `[index] = 1`.
        if p.current_token() == CppTokenKind::Dot
            || p.current_token() == CppTokenKind::LeftBracket
        {
            let designator = p.mark(CppSyntaxKind::DesignatedInitExpr);
            if p.current_token() == CppTokenKind::Dot {
                p.bump();
                if p.current_token() == CppTokenKind::Identifier {
                    p.bump();
                }
            } else {
                p.bump();
                let _ = parse_expr(p);
                let _ = expect_token(p, CppTokenKind::RightBracket);
            }
            if p.current_token() == CppTokenKind::Assign {
                p.bump();
            }
            if let Err(err) = parse_initializer_clause(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            designator.complete(p);
        } else if let Err(err) = parse_initializer_clause(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
        } else {
            break;
        }
    }

    expect_token(p, CppTokenKind::RightBrace)?;
    Ok(m.complete(p))
}

/// Parse a constructor member initializer list: `: a(1), b{2}, c()`.
fn parse_member_initializer_list(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();

    expect_token(p, CppTokenKind::Colon)?;

    loop {
        let m = p.mark(CppSyntaxKind::MemberInitializer);
        if let Err(err) = parse_name(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        match p.current_token() {
            CppTokenKind::LeftParen => {
                if let Err(err) = parse_expression_list(p, CppTokenKind::RightParen) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            CppTokenKind::LeftBrace => {
                if let Err(err) = parse_braced_initializer(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            // A pack expansion: `Base(args)...`
            CppTokenKind::Ellipsis => p.bump(),
            _ => {}
        }
        m.complete(p);

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            continue;
        }
        break;
    }

    Ok(CompleteMarker::empty())
}

/// Parse `( expr, expr, ... )`, consuming the closing delimiter.
pub fn parse_expression_list(p: &mut CppParser, closing: CppTokenKind) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ArgumentList);
    let opening = if closing == CppTokenKind::RightParen {
        CppTokenKind::LeftParen
    } else {
        CppTokenKind::LeftBracket
    };

    expect_token(p, opening)?;

    while p.current_token() != closing && !p.is_eof() {
        if let Err(err) = parse_expr(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            continue;
        }
        break;
    }

    expect_token(p, closing)?;
    Ok(m.complete(p))
}

/// Parse a function parameter list, including the `(...)` of a variadic function.
pub fn parse_parameter_list(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ParameterList);

    expect_token(p, CppTokenKind::LeftParen)?;

    // `f()` — no parameters at all.
    if p.current_token() == CppTokenKind::RightParen {
        p.bump();
        return Ok(m.complete(p));
    }

    // `f(...)` — an old-style variadic function. The lexer produces one `...` token, but a stray
    // `...` can also follow named parameters.
    if p.current_token() == CppTokenKind::Ellipsis {
        p.bump();
        expect_token(p, CppTokenKind::RightParen)?;
        return Ok(m.complete(p));
    }

    loop {
        if let Err(err) = parse_parameter(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        match p.current_token() {
            CppTokenKind::Comma => {
                p.bump();
                // A trailing `...` after the last named parameter.
                if p.current_token() == CppTokenKind::Ellipsis {
                    p.bump();
                    break;
                }
                continue;
            }
            _ => break,
        }
    }

    expect_token(p, CppTokenKind::RightParen)?;
    Ok(m.complete(p))
}

/// Parse one parameter: a type, an optional declarator, and an optional default argument.
fn parse_parameter(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Parameter);

    if let Err(err) = parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The declarator is optional: `void f(int)` is as valid as `void f(int x)`.
    if !definitely_ends_a_type(p.current_token())
        && p.current_token() != CppTokenKind::Comma
        && let Err(err) = parse_declarator(p) {
            p.close_marks_above(base);
            return Err(err);
        }

    if p.current_token() == CppTokenKind::Assign {
        p.bump();
        let init = p.mark(CppSyntaxKind::Initializer);
        if let Err(err) = parse_initializer_clause(p) {
            init.undo(p);
            p.close_marks_above(base);
            return Err(err);
        }
        init.complete(p);
    }

    Ok(m.complete(p))
}

/// Parse the body of a class, struct or union: `{ public: ... };`.
pub fn parse_class_body(p: &mut CppParser) -> ParseResult {
    let _base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ClassBody);

    expect_token(p, CppTokenKind::LeftBrace)?;

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        let member_base = p.open_marks();

        if matches!(
            p.current_token(),
            CppTokenKind::PublicKeyword
                | CppTokenKind::PrivateKeyword
                | CppTokenKind::ProtectedKeyword
        ) {
            parse_access_specifier(p)?;
            continue;
        }

        // A member is a declaration; anything that is not gets wrapped in an error node so the
        // loop always advances.
        let before = p.current_token_index();
        if parse_member(p).is_err() {
            p.close_marks_above(member_base);
            if p.current_token_index() == before {
                let error = p.mark(CppSyntaxKind::ErrorNode);
                p.bump();
                error.complete(p);
            }
        }
    }

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        // An unclosed class body is the normal state of a file being edited.
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

fn access_specifier_kind(kind: CppTokenKind) -> CppSyntaxKind {
    match kind {
        CppTokenKind::PublicKeyword => CppSyntaxKind::PublicAccess,
        CppTokenKind::PrivateKeyword => CppSyntaxKind::PrivateAccess,
        _ => CppSyntaxKind::ProtectedAccess,
    }
}

/// Parse `public:`, `private:` or `protected:` — the keyword and its colon, and nothing else.
///
/// # Why the node is closed by hand
///
/// `bump` attaches the trivia *after* a token to whichever node is open, which is usually what you
/// want: `int  x` keeps its spaces together. For an access specifier it is not. Everything after
/// `public:` — the newline, an indented `/// doc`, the member below — would then be a child of the
/// access specifier rather than of the class body, and a consumer asking "which comment documents
/// this member?" would find nothing, because the only comment in the class body is the one the
/// access specifier swallowed.
///
/// So the keyword and colon are consumed directly and the node is closed before the trailing trivia
/// is emitted. The trivia still goes into the tree — it is the class body's — which is what keeps
/// the CST lossless.
fn parse_access_specifier(p: &mut CppParser) -> ParseResult {
    let access = p.mark(access_specifier_kind(p.current_token()));
    p.consume_current_token();
    p.consume_current_token_if(CppTokenKind::Colon);
    access.complete(p);

    // Whatever layout followed the colon belongs to the enclosing block, so it is emitted now that
    // the access node is closed. Only trivia can be here: `bump` is what emits it, and it has not
    // been called since the colon.
    p.emit_trivia_after_current_token();

    Ok(CompleteMarker::empty())
}

/// One class member: a member declaration, or a nested definition.
fn parse_member(p: &mut CppParser) -> ParseResult {
    // A nested class/struct/enum definition, or any other declaration, is handled uniformly by the
    // declaration parser — a member declaration *is* a declaration.
    parse_declaration(p)
}

/// Consume a `;`, or report it as missing without consuming anything.
pub fn expect_semicolon(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
        return Ok(CompleteMarker::empty());
    }

    p.emit_missing_node();
    Err(CppParseError::syntax_error_from(
        "expected `;`",
        p.current_token_range(),
    ))
}

/// Is the current token the start of something that can only be a declaration?
///
/// Used to skip the speculative pass when the answer is obvious. Being conservative is safe: a
/// `false` here only costs a backtrack.
pub fn starts_declaration(p: &CppParser) -> bool {
    match p.current_token() {
        CppTokenKind::TypedefKeyword
        | CppTokenKind::UsingKeyword
        | CppTokenKind::NamespaceKeyword
        | CppTokenKind::TemplateKeyword
        | CppTokenKind::ExternKeyword
        | CppTokenKind::StaticAssertKeyword
        | CppTokenKind::ClassKeyword
        | CppTokenKind::StructKeyword
        | CppTokenKind::UnionKeyword
        | CppTokenKind::EnumKeyword
        | CppTokenKind::ConceptKeyword => true,

        // These are specifiers, but several of them also begin expressions: `const` cannot,
        // `static` cannot, `auto` cannot — while `decltype(x)` and `noexcept(...)` can.
        CppTokenKind::ConstKeyword
        | CppTokenKind::VolatileKeyword
        | CppTokenKind::ConstexprKeyword
        | CppTokenKind::StaticKeyword
        | CppTokenKind::InlineKeyword
        | CppTokenKind::VirtualKeyword
        | CppTokenKind::ExplicitKeyword
        | CppTokenKind::FriendKeyword
        | CppTokenKind::MutableKeyword
        | CppTokenKind::ThreadLocalKeyword => true,

        _ => false,
    }
}

/// Parse a `using` declaration or directive, and `using` aliases.
pub fn parse_using_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::UsingDecl);

    expect_token(p, CppTokenKind::UsingKeyword)?;

    if matches!(p.current_token_text(), "namespace") {
        let directive = p.mark(CppSyntaxKind::UsingDirective);
        p.bump();
        parse_name(p)?;
        let _ = expect_semicolon(p);
        directive.complete(p);
        return Ok(m.complete(p));
    }

    // `using Alias = Type;`
    if let Err(err) = parse_name(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if p.current_token() == CppTokenKind::Assign {
        p.bump();
        if let Err(err) = parse_type_id(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    if let Err(err) = expect_semicolon(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a `typedef` declaration: `typedef int MyInt;`.
pub fn parse_typedef_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypedefDecl);

    expect_token(p, CppTokenKind::TypedefKeyword)?;

    // The rest is a declaration without the keyword, so reuse the same machinery.
    if let Err(err) = super::types::parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = super::types::parse_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = expect_semicolon(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a `static_assert(...)` declaration.
pub fn parse_static_assert(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::DeclStat);

    expect_token(p, CppTokenKind::StaticAssertKeyword)?;

    if p.current_token() == CppTokenKind::LeftParen {
        p.bump();
        if let Err(err) = parse_expr(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        // The message is optional in C++17 and later.
        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            if let Err(err) = parse_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        if let Err(err) = expect_token(p, CppTokenKind::RightParen) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    if let Err(err) = expect_semicolon(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a namespace definition: `namespace a::b { ... }` or `namespace a = b;`.
pub fn parse_namespace_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::NamespaceDecl);

    expect_token(p, CppTokenKind::NamespaceKeyword)?;

    // A namespace alias: `namespace fs = std::filesystem;`
    if p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::Assign
    {
        p.bump();
        p.bump();
        parse_name(p)?;
        let _ = expect_semicolon(p);
        return Ok(m.complete(p));
    }

    // An anonymous namespace.
    if p.current_token() != CppTokenKind::LeftBrace {
        // `inline` before the name is part of the declaration; the parser reaches this with it
        // already consumed as a specifier, so an identifier here is the name.
        if p.current_token() == CppTokenKind::Identifier {
            parse_name(p)?;
        }
    }

    if p.current_token() == CppTokenKind::LeftBrace {
        if let Err(err) = parse_stats_block(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    let _ = expect_semicolon(p);
    Ok(m.complete(p))
}

/// Parse a `{ ... }` block whose contents are declarations, reusing the statement machinery.
fn parse_stats_block(p: &mut CppParser) -> ParseResult {
    let _base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    expect_token(p, CppTokenKind::LeftBrace)?;
    parse_stats(p);

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}
