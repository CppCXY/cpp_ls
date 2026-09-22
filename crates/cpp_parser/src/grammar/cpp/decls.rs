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
//! a * b;               declaration of `b` as a pointer to `a`? or multiplication?
//! T(x);                declaration of `x`? or a functional cast / function call?
//! Widget w(1, 2, 3);   declaration of `w`? or a call to `w` with three arguments?
//! ```
//!
//! There is no symbol table in a parser that must work on a file being edited, so the question is answered
//! *speculatively*: [`parse_declaration`] tries the declaration reading, and if it does not land on a
//! plausible end (a `;`, a `{`, or an initializer), the caller rewinds and treats the tokens as an
//! expression. [`super::stats::parse_declaration_or_expression_statement`] is that caller.
//!
//! # What breaks the tie inside the declaration reading
//!
//! The backtrack settles *whether* a statement is a declaration. It cannot settle where the type ends and the
//! declarator begins, which is a decision the declaration reading has to make on its own — and that decision
//! is what [`declarator_starts_with_a_known_type_name`], [`the_arguments_look_like_values`] and
//! [`the_arguments_look_like_declarators`] answer between them. In order of strength:
//!
//! 1. **A keyword type.** `int a(1);` — no expression begins with `int`.
//! 2. **A name the file declares to be a type.** `struct Widget {}; Widget w(1, 2);` — see
//!    [`crate::parser::TypeNames`].
//! 3. **A name, then a second name, then parentheses holding values.** `Widget w(1, 2, 3);` — the shape no
//!    call has. The second name is the declarator and the parentheses are its initializer.
//! 4. **File scope, with parentheses holding names.** `Max(a, b);` — a statement at file scope is not a
//!    call, and bare names are what an initializer most often copies.
//!
//! Each rule is strictly weaker than the one before it, and each is refused rather than guessed when the
//! evidence is absent: a call read as a declaration loses its callee *and* its arguments, while a declaration
//! read as a call merely leaves a variable unbound. That asymmetry is the whole design, and it is why
//! `A(B);` stays a call while `Widget w(1, 2, 3);` is a declaration.
//!
//! The cost is one re-parse of a statement whose first token is ambiguous, and a handful of documented
//! readings given up — `tests/gaps.rs` lists them, and `tests/direct_init.rs` pins the rules themselves.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CompleteMarker, CppParser, Marker, MarkerEventContainer, ParseAnchor},
    parser_error::CppParseError,
};

use super::{
    expect_token,
    exprs::parse_expr,
    stats::{parse_compound_stat, parse_stats},
    types::{
        definitely_ends_a_type, is_type_specifier_keyword, parse_decl_specifier_seq,
        parse_declarator, parse_declarator_with, parse_name, parse_type_id,
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
    if let Err(err) = parse_template_parameter_list(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a template parameter list: the `<` … `>` of `template <typename T>`, and of a C++20 generic
/// lambda's `[]<typename T>`.
///
/// Shared by [`parse_template_head`] and the lambda rule in the expression grammar, because the two
/// spell the same list and only differ in the keyword in front of it — a lambda has none, its capture
/// list stands where `template` does. The list is the part with the rules in it (`>>` splitting, packs,
/// constrained parameters, defaults), so a second copy for lambdas is exactly how generic lambdas would
/// come to disagree with class templates about what a parameter list is.
pub fn parse_template_parameter_list(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateParameterList);

    expect_token(p, CppTokenKind::Less)?;

    // `template <>` — an explicit specialization has an empty list.
    if p.current_token() == CppTokenKind::Greater {
        p.bump();
        return Ok(m.complete(p));
    }

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

    Ok(m.complete(p))
}

/// Does this token end the part of a template parameter that a declarator could occupy?
///
/// [`definitely_ends_a_type`] plus the two tokens a parameter's *list* provides: a `,` before the next
/// parameter, and the pack marker of `typename... Rest`. A `>` would end it too, and is already in the set —
/// `>>` is not, because by the time the list is being walked a nested closer has been split.
fn ends_a_template_parameter_head(kind: CppTokenKind) -> bool {
    definitely_ends_a_type(kind)
        || matches!(
            kind,
            CppTokenKind::Ellipsis | CppTokenKind::Greater
        )
}

/// Parse one template parameter: `typename T`, `class C`, `int N`, `template <...> class T`,
/// `T...`, or a constrained parameter `C T`.
fn parse_template_parameter(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateParameter);

    // A template template parameter: `template <typename> class C`.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && let Err(err) = parse_template_head(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    // `typename... Rest` — the one spelling that cannot go through the declaration machinery below, and the
    // reason is worth recording because the failure it caused was not where it looked. The specifier sequence
    // sees `typename`, then the `...`, which is not a specifier — so it stops, having consumed only the
    // keyword, and reports the ellipsis as an unreadable specifier while the *parameter* marker ends up at
    // `Rest`. What came out was `expected ',' or '>'` against a name the list had never asked for. Consuming
    // both keywords together here keeps the two halves of the pack in one rule.
    //
    // `class... Ts` and `int... Ns` do not need this: there the marker follows a type that is one keyword, and
    // the specifier loop claims the pair through the pack branch in
    // [`super::types::parse_one_decl_specifier_inner`].
    if p.current_token() == CppTokenKind::TypenameKeyword
        && p.peek_next_token() == CppTokenKind::Ellipsis
    {
        let typename_type = p.mark(CppSyntaxKind::TypenameType);
        p.bump(); // `typename`
        p.bump(); // `...`
        typename_type.complete(p);

        if p.current_token() == CppTokenKind::Identifier
            && let Err(err) = parse_name(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // The parameter itself is a declaration: `typename T`, `int N`, `auto N`, and a constrained
    // parameter is a type followed by a name. Reusing the declaration machinery keeps the shapes
    // the same as everywhere else.
    if let Err(err) = parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The element name, and a pack expansion marker. The ellipsis is excluded because a parameter can end in
    // one with no name before it — `int... Ns` — and the declarator rule has nothing to read there.
    if !ends_a_template_parameter_head(p.current_token())
        && p.current_token() != CppTokenKind::Ellipsis
        && let Err(err) = parse_declarator(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    if p.current_token() == CppTokenKind::Ellipsis {
        p.bump();

        // The pack's *name* follows the marker in three of the four spellings — `class... Ts`, `int... Ns`,
        // `typename... Rest` — and only `T...` puts it first. Consuming the marker therefore leaves the
        // parameter unfinished, and what happens next is the name being left behind: the list loop finds an
        // identifier where it expects `,` or `>` and reports the parameter as malformed. The two spellings are
        // indistinguishable at this point, so the rule is the same for both: a name here belongs to the pack.
        if p.current_token() == CppTokenKind::Identifier
            && let Err(err) = parse_name(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }
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
        // A using-declaration or alias — `using ns::f;`, `using Alias = T;`.
        //
        // Dispatched here rather than left to the general rule, which begins with a specifier and would read
        // `using` as a type. At file scope the statement rule happens to claim it first, which is why this gap
        // was invisible until a *class member* was written the same way: `using Base::method;` inside a class
        // came out as an error node holding the keyword and a declaration of a type called `Base::method`.
        CppTokenKind::UsingKeyword => return parse_using_declaration(p),
        CppTokenKind::ExternKeyword if starts_a_linkage_specification(p) => {
            return parse_linkage_specification(p);
        }
        // A destructor — `~S();`, `~S() {}`, `virtual ~S() = default;`.
        //
        // Dispatched here rather than left to the general rule because the tilde is *part of the name* and the
        // specifier sequence, which runs first, sees it as a unary operator where a type should be: the whole
        // declaration was refused, the tilde was wrapped in an error node on its own, and what followed was
        // read as a declaration of `S` with a parameter list and nothing named.
        CppTokenKind::Tilde => return parse_destructor_declaration(p),

        // `export` heads a module declaration, an export block, or one exported declaration.
        CppTokenKind::ExportKeyword => {
            if p.peek_token_kind_at(1..2)[0] == CppTokenKind::LeftBrace {
                return super::modules::parse_export_block(p);
            }

            // Everything else is `export declaration`, and the declaration is parsed by the ordinary rule —
            // which is what puts the `export` *inside* the declaration node, so that a consumer reading a
            // declaration's own tokens can see that it is exported. Consuming the keyword here and then
            // wrapping the result would put it outside instead: a token is emitted at the moment it is
            // consumed, and by then the wrapper had not been opened yet.
            //
            // `module` and `import` are contextual keywords — they arrive as identifiers — so the word after
            // `export` is what says whether this is a module declaration or a re-export. Recognised by
            // spelling, which is what [`super::modules::parse_exported_declaration`] does.
            if matches!(p.peek_token_text_at(1), "module" | "import") {
                return super::modules::parse_exported_declaration(p);
            }

            // `using`, `namespace`, `typedef` and `static_assert` have rules of their own, and the general
            // rule below cannot start from any of them: it begins with a specifier, and none of those four
            // keywords is one. `export using Point = shapes::Point;` — an everyday line in a module interface —
            // would be read as an expression and reported as broken. Dispatched *after* the keyword so that it
            // is consumed by the rule that owns the construct, which is where those rules expect to start.
            match p.peek_token_kind_at(1..2)[0] {
                CppTokenKind::UsingKeyword => {
                    p.bump();
                    return parse_using_declaration(p);
                }
                CppTokenKind::NamespaceKeyword => {
                    p.bump();
                    return parse_namespace_declaration(p);
                }
                CppTokenKind::TypedefKeyword => {
                    p.bump();
                    return parse_typedef_declaration(p);
                }
                CppTokenKind::StaticAssertKeyword => {
                    p.bump();
                    return parse_static_assert(p);
                }
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

    // The `export` of `export int helper();`, consumed *inside* the declaration node rather than at the
    // dispatch above. A token is emitted at the moment it is consumed, so an `export` consumed before this
    // marker existed would land beside the declaration instead of in it — and "is this declaration exported?"
    // is answered by reading the declaration's own tokens, which is the whole reason the spelling exists.
    if p.current_token() == CppTokenKind::ExportKeyword {
        p.bump();
    }

    // A template head wraps whatever declaration follows it: `template <typename T> struct V {};`
    // is a template *declaration* whose payload is the class. Handling it here rather than in a
    // separate rule is what lets templates apply to classes, functions, variables, aliases and
    // concepts without five copies of the same parser.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && let Err(err) = parse_template_head(p)
    {
        p.rollback(checkpoint);
        return Err(err);
    }

    // The head may be followed by a declaration that *is* its own rule, and those rules begin with a
    // keyword the specifier sequence cannot read: `template <typename U> using rebind = Rebind<U>;` is an
    // alias template, and `using` is not a type specifier — the sequence refused it and the declaration
    // was reported as `expected a type specifier` against the `using` itself.
    //
    // Dispatched *after* the head rather than by the match at the top of this function, because at the top
    // the cursor is on the `template` keyword. The rule is called directly rather than by re-entering this
    // function: the declaration marker is already open above, and a second one would leave this call's own
    // marker unpaired — which the translation-unit assertion reports as a rule that unwound past its owner.
    if p.current_token() == CppTokenKind::UsingKeyword {
        if let Err(err) = parse_using_declaration(p) {
            p.rollback(checkpoint);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    if p.current_token() == CppTokenKind::TypedefKeyword {
        if let Err(err) = parse_typedef_declaration(p) {
            p.rollback(checkpoint);
            return Err(err);
        }
        return Ok(m.complete(p));
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

    // The same thing, one level in: a **`friend` declaration is a specifier whose payload is the whole
    // declaration**, `;` included. `friend void swap(A&, A&);` therefore leaves the cursor on the *next*
    // member with the statement already finished — indistinguishable from an empty specifier sequence by the
    // cursor alone, which is why the outer rule looked for an init-declarator that was not there, failed, and
    // rewound: `friend` ate every member written after it, and the recovery turned them into error nodes.
    //
    // The specifier sequence reports that it finished a statement, which no other specifier ever does.
    if p.take_declaration_ended_inside_specifiers() {
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
    if p.current_token() == CppTokenKind::Semicolon
        && declaration_defined_a_class(p, specifiers_from)
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

        // A binding pattern is never a function declarator, so the flag has to be cleared rather than left as
        // whatever the previous declarator set. It is sticky by design — an empty parameter list leaves no
        // event behind for a caller to look for — and a stale `true` here is what made
        // `for (auto [a, b] : pairs)` consume `: pairs` as a constructor's member initializer list.
        p.set_last_declarator_is_function(false);

        return finish_init_declarator(p, m, declarator_from);
    }

    // `T(x);` and `int (x);` are parenthesized declarators, not calls. Detecting that here is what
    // keeps the declarator reading from mis-nesting the parentheses.
    //
    // Only when the specifier sequence saw no *name*, though. Once it has, the type is written and everything
    // after it is the declarator, so a `(` there cannot be a declarator of its own — `Max(a);` is a statement
    // whose parentheses hold an argument, and reading them as a parenthesized name `a` invented a variable
    // declaration out of a call. A keyword type is the other way round: `int` is never the declarator, so the
    // parentheses are all that is left to hold one.
    //
    // Direct-initialisation — `Widget widget(1, 2);` — is **not** handled here, and the reason is worth
    // recording because the obvious fix does not work. Reading the argument list as an initializer whenever the
    // parentheses hold something other than a single identifier looks right and does make
    // `Widget widget(1, 2);` parse; it also takes `g(1, 2);` away from the expression reading. The specifier
    // sequence takes `g` for a type, this branch consumes `(1, 2)` as an initializer, and the declaration then
    // declares nothing — so a call statement loses its callee *and* its arguments, which the existing
    // `CallExpr` test caught immediately.
    //
    // It is handled by the declarator's own suffix loop instead, which is where the decision can be made from
    // the whole statement rather than from the parentheses alone: the loop runs because the declarator has a
    // name — `Widget w(…)` — or because the arguments look like declarators at file scope, and never for the
    // single-name shape a call has.
    if p.current_token() == CppTokenKind::LeftParen && !p.has_declaration_type_name() {
        let checkpoint = p.checkpoint();
        let paren = p.mark(CppSyntaxKind::Declarator);
        p.bump();
        // A parenthesized declarator has to *hold* a declarator, and an empty one is not enough. `int (x)`
        // holds a name and `int (*p)(int)` a pointer; the `(` of `g()` holds neither, because the specifier
        // sequence has taken `g` for a type and there is nothing left to name. Without this the empty reading
        // succeeds, the parentheses are consumed as a declarator, and a call statement becomes a declaration of
        // nothing — losing the callee, the arguments, and the expression.
        let named_something = starts_a_declarator(p) && parse_declarator(p).is_ok();
        if !named_something {
            // Not a parenthesized declarator; let the expression reading have it.
            p.rollback(checkpoint);
        } else if p.current_token() == CppTokenKind::RightParen {
            p.bump();
            paren.complete(p);

            // `S()` is a function declarator, and this branch is the one that recognises it: the empty
            // parentheses are consumed here rather than by `parse_parameter_list`, so the flag that
            // `parse_declarator` would have set has to be set here too. Without it a constructor's member
            // initializer list — `S() : a(1) {}` — is not recognised, because the `:` looks like it follows a
            // variable.
            //
            // Safe for the variable reading (`int (x)`, a parenthesized declarator) because the two are
            // distinguished by what follows: a name cannot be followed by `:`, so the flag is only ever
            // consulted on a path where the function reading is the right one.
            p.set_last_declarator_is_function(true);

            // The qualifiers that follow a parameter list, which the suffix loop below does not know about: it
            // continues on `(` and `[`, and `noexcept` is neither. Leaving them to the caller is how
            // `C() noexcept : x(0)` ended up with the `noexcept` unread and the member initializer list
            // unrecognised — the two mistakes are the same one.
            super::types::eat_function_qualifiers(p);

            if let Err(err) = finish_declarator_suffixes(p, declarator_from) {
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
            &[
                CppSyntaxKind::ParameterList,
                CppSyntaxKind::TrailingReturnType,
            ],
        );

    match p.current_token() {
        CppTokenKind::Assign => {
            p.bump();

            // `= default` and `= delete` — a *defaulted* or *deleted* function definition. Neither keyword
            // can begin an expression, so the initializer rule has nothing to read and would report
            // `expected primary expression` against the one token that makes a modern comparison operator
            // work: `bool operator==(const D&) const = default;` is how C++20 writes one.
            //
            // Kept inside the `Initializer` node rather than consumed as a bare token, because that is what it
            // is: the initializer of a declarator, spelled with a keyword instead of an expression.
            if matches!(
                p.current_token(),
                CppTokenKind::DefaultKeyword | CppTokenKind::DeleteKeyword
            ) {
                let init = p.mark(CppSyntaxKind::Initializer);
                p.bump();
                init.complete(p);
                return Ok(m.complete(p));
            }

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
        // A constructor's member initializer list: `Foo() : a(1), b(2) {}`. Only a *function* declarator can be
        // followed by one, so a `:` after a variable belongs to whatever encloses it.
        //
        // This is what a range-based `for`'s separator is: `for (auto x : items)` has a `:` right after the
        // declared variable, and reading it as a member initializer list swallowed the whole range and then
        // reported "expected `;`" against the header's `)`.
        //
        // The flag alone decides it, because `parse_declarator` sets it on **every** path — false at the start,
        // true when a parameter list is parsed, including an empty one. Consulting the event stream as well looks
        // like belt and braces and is not: an empty parameter list leaves no node behind, so `Foo() : a(1)` would
        // be read as a variable declaration and its member initializer list rejected.
        CppTokenKind::Colon if p.last_declarator_is_function() => {
            parse_member_initializer_list(p)?;
        }
        // A bit-field: `int bits : 3;`, `unsigned flags : 1, spare : 7;`.
        //
        // The same `:` in the same position as a member-initializer list, and the enclosing construct is what
        // tells them apart: a constructor's `:` follows a *function* declarator, which the arm above has already
        // claimed, while a bit-field's follows a member that names no function and sits inside a class body.
        //
        // Read as a member-initializer list, as it was, `int bits : 3` came out as a declarator with no name at
        // all: the width was consumed as an initializer, the member was nameless, and a consumer asking the
        // class for its fields found nothing where the field was.
        CppTokenKind::Colon if p.is_in_class_body() => {
            let width = p.mark(CppSyntaxKind::Initializer);
            p.bump(); // `:`

            // The width is a constant-expression, and it is optional in the grammar's own terms only because a
            // nameless bit-field is written `int : 0` — the `:` is never followed by a `;`.
            if let Err(err) = parse_expr(p) {
                width.undo(p);
                return Err(err);
            }
            width.complete(p);
        }
        _ => {}
    }

    Ok(m.complete(p))
}

/// Continue a declarator after a parenthesized name has been consumed.
///
/// `declarator_from` is the event index where that declarator began, forwarded to
/// [`parse_function_suffix_or_initializer`] so the rule that decides between a parameter list and a
/// direct-initialiser can ask whether the declarator named anything.
fn finish_declarator_suffixes(p: &mut CppParser, declarator_from: usize) -> ParseResult {
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                // The same decision the main declarator loop makes, through the same function: a `(` after a
                // declared name is a parameter list or the parentheses of a direct-initialised variable. Having
                // two copies of that rule is how they come to disagree.
                parse_function_suffix_or_initializer(p, declarator_from)?;
                if p.current_token() == CppTokenKind::LeftParen {
                    return Ok(CompleteMarker::empty());
                }
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                if p.current_token() != CppTokenKind::RightBracket
                    && !p.is_eof()
                    && let Err(err) = parse_expr(p)
                {
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

/// A `(` after a declared name: a parameter list, or the parentheses of a direct-initialised variable.
///
/// The two are told apart by what the parentheses can hold:
///
/// ```text
/// int a(b);        a parameter list, because `b` parses as a type — hence a function
/// int a(1);        not a parameter list: `1` is not a type, so this is a variable initialised with `1`
/// Widget w(1, 2);  likewise — a variable initialised with two arguments
/// ```
///
/// The parameter reading is tried before the initializer reading in most cases, because it is the one that can
/// fail on its own: a parameter must have a type, and a literal does not. A failed attempt is rewound, which is
/// the same bounded backtracking [`crate::grammar::cpp::types::parse_declarator`] already relies on for the same
/// ambiguity. Trying the initializer reading first would take `int a(b)` away from the function reading, which is
/// the reading C++ gives it.
///
/// **An untyped list of names is the exception**, and it is tried the other way round: `a` is a valid parameter
/// type as well as a valid argument, so the parameter reading succeeds on `Max(a, b);` and would never be
/// revisited. At file scope that shape is a declaration, so the initializer gets first refusal there.
///
/// Returns whether it consumed anything, so a caller that reaches this on an expression can leave the
/// parentheses alone.
///
/// `declarator_from` is the event index where the declarator being suffixed began, so the question "did that
/// declarator name anything?" can be asked of the events it produced. See
/// [`CppParser::events_contain_any_between`] for why the answer has to come from the event stream rather than
/// from a parameter.
pub fn parse_function_suffix_or_initializer(p: &mut CppParser, declarator_from: usize) -> ParseResult {
    let checkpoint = p.checkpoint();

    // An untyped list of names has to be claimed *before* the parameter reading is tried, and only here. A
    // parameter must have a type, but a bare name is also a perfectly good type, so `Max(a, b);` parses as a
    // parameter list — two parameters of type `a` and type `b` — and a reading that succeeds is never revisited.
    // At file scope that is the wrong answer for a shape that only a declaration can have, so the initializer
    // reading goes first and the parameter reading is what gets rewound instead. Inside a body the call is the
    // ordinary reading and the order is left alone; see [`a_declaration_is_the_better_reading`].
    //
    // The second form is the same argument one level down: once the leading name *is* a type this file declared,
    // the parentheses follow a declarator name, and `Inner(1)` in them is a value being constructed rather than
    // a parameter. That reading is what a parameter list gets wrong — it takes `Inner` for a parameter's type
    // and then reads `(1)` as that parameter's default argument, which is a declaration of a function nobody
    // wrote.
    let declarator_is_named = p.has_declaration_type_name();
    if (p.is_at_file_scope() && the_arguments_look_like_declarators(p, false)
        || declarator_is_named && the_arguments_look_like_declarators(p, true))
        && let Ok(parsed) = parse_the_initializer(p)
    {
        return Ok(parsed);
    }

    if parse_parameter_list(p).is_ok() {
        // A parameter list is what makes a declarator a function declarator. Recorded here because an *empty*
        // one leaves no node for a later event-stream check to find.
        p.set_last_declarator_is_function(true);
        super::types::eat_function_qualifiers(p);
        return Ok(CompleteMarker::empty());
    }

    p.rollback(checkpoint);

    // Direct-initialisation, when the evidence says this is a declaration rather than a call. The two are the
    // same tokens:
    //
    // ```text
    // int a(1);        a declaration: `int` is a keyword type
    // Widget w(1, 2);  a declaration, because `Widget` is a type this file declares
    // g(1, 2);         a call, because `g` is not
    // ```
    //
    // The question is settled by asking what the tokens cannot answer — whether the type name is one — using the
    // file's own declarations, plus the scope the statement sits in. See [`a_declaration_is_the_better_reading`].
    if a_declaration_is_the_better_reading(p, declarator_from) && an_argument_list_follows(p) {
        return parse_the_initializer(p);
    }

    Ok(CompleteMarker::empty())
}

/// Read the argument list at the cursor as a direct-initialiser.
fn parse_the_initializer(p: &mut CppParser) -> ParseResult {
    let init = p.mark(CppSyntaxKind::Initializer);
    if let Err(err) = parse_expression_list(p, CppTokenKind::RightParen) {
        init.undo(p);
        return Err(err);
    }
    init.complete(p);
    Ok(CompleteMarker::empty())
}

/// Should `T x(...)` be read as a declaration rather than as a call?
///
/// Two kinds of evidence, both from the file being parsed, and both about what real code looks like rather than
/// about what the grammar permits.
///
/// # The type name
///
/// A declaration begins with a type, and a call begins with a callee. If the leading name is one this file
/// declared — `class Widget`, `typedef ... Integer`, `using Alias = ...` — the declaration reading is the one a
/// compiler would take. [`crate::parser::TypeNames`] is the table; it records only what a declaration spells out,
/// so a miss costs the declaration reading rather than producing a wrong one.
///
/// # The scope
///
/// **A function declaration inside a function body is vanishingly rare**, while a local variable with
/// constructor arguments is everywhere. So the operators, known type names, and member functions declared
/// *before* the current one are the real cases. The parser's brace depth is a good proxy for this: at depth
/// zero the file is declaring things, and inside a body it is writing code. Recording too much depth would
/// make a nested block look like a new file scope, so a single level is treated as "inside a body".
///
/// The direction of the guess matters and is the reason this is worth doing at all: reading a **call** as a
/// declaration loses its callee *and* its arguments, and puts a name in scope that was never declared — so a
/// bare name with no type to back it stays a call. Reading a **declaration** as a call merely leaves the
/// variable unbound, which is the cheaper mistake, and it is the one made when neither signal is present.
pub fn a_declaration_is_the_better_reading(p: &CppParser, declarator_from: usize) -> bool {
    // A keyword type can only begin a declaration: no expression starts with `int`.
    if declarator_starts_with_a_type_keyword(p) {
        return true;
    }

    // A name this file declared to be a type, wherever the declaration is written. Inside a body this is the
    // common case — `Widget w(1, 2);` — and it is also what makes `Widget make();` a declaration at file scope.
    if declarator_starts_with_a_known_type_name(p) {
        return p.is_inside_a_body() || p.is_at_file_scope();
    }

    // Nothing in the file says the leading name is a type — it was declared in a header, or below the point
    // being edited — but the declarator **named** something and the list at the cursor is unambiguously an
    // argument list rather than a parameter list. Together those are the shape of direct-initialisation and of
    // nothing else:
    //
    // ```text
    // Widget w(1, 2, 3);   a variable: `w` is named, and `1` cannot be a parameter's type
    // Widget w("name");    likewise for a string
    // g(1, 2, 3);          a call: no name was parsed after the type, so there is nothing to initialise
    // A(B);                likewise — `B` was left outside the type as an argument, not taken as a name
    // ```
    //
    // The two `true`s are a conjunction and neither is disposable. The name is what separates `Widget w(1)` from
    // `g(1)`, and the values are what separate `Widget w(1)` from the *parameter* list of a function declaration
    // whose return type this file has not seen — the reading [`parse_parameter_list`] has already tried and
    // failed on by the time this is asked.
    if a_name_was_parsed(p, declarator_from) && the_arguments_look_like_values(p) {
        return true;
    }

    // Nothing in the file says the leading name is a type, but the arguments do not look like values either.
    // A list of bare identifiers is what a declaration's *initializer* most often looks like — `Max(a, b)` copies
    // two things that are already in scope — while a call passes values, and a value is written as a literal, a
    // call, a member access or an operator. See [`the_arguments_look_like_declarators`].
    //
    // The scope gate is what keeps this off expressions. At file scope a statement cannot be a call, so the
    // reading costs nothing; inside a body a call is the ordinary thing to find, and the guess is not worth
    // making there. Reading a declaration as a call leaves a variable unbound and is recoverable; reading a
    // call as a declaration loses its callee and invents a binding, so the weaker the evidence the less of the
    // file this is allowed to apply to.
    p.has_declaration_type_name()
        && p.is_at_file_scope()
        && the_arguments_look_like_declarators(p, false)
}

/// Did the declarator that began at `declarator_from` name anything?
///
/// The question [`a_declaration_is_the_better_reading`] has to ask and cannot answer from the type name alone:
/// in both `Widget w(1)` and `g(1)` a name was recorded in type position — `Widget` and `g` — so
/// [`CppParser::declaration_type_name`] is `Some` either way, and it is the *second* name that decides.
///
/// Read off the event stream rather than tracked as parser state, because the events are what the tree will be
/// built from: a name that was parsed and then rolled back is not in them, and a name that is in them is one the
/// declarator really has. The lower bound is what keeps the type's own `NameExpr` — recorded while the specifier
/// sequence ran, before this point — out of the answer.
fn a_name_was_parsed(p: &CppParser, declarator_from: usize) -> bool {
    p.events_contain_any_between(
        declarator_from,
        p.current_event_count(),
        &[CppSyntaxKind::NameExpr],
    )
}

/// Does the parenthesised list at the cursor hold values rather than declarations?
///
/// The question that separates a direct-initialiser from a parameter list **without** a type table, and the
/// companion to [`the_arguments_look_like_declarators`]: that one asks whether a list of names is a list of
/// things being copied, while this one asks whether the list contains anything that can only be a value.
///
/// The rule is about the **start of each element**, which is the one position where a declarator and an
/// expression cannot be confused: a parameter begins with a type, and no type is written as a literal, as a
/// name that is immediately called, or as an operator that no type contains.
///
/// * a **literal** — `1`, `"name"`, `'c'`, `true`, `nullptr`. A parameter is a type and a declarator, and
///   neither is written as a literal; `Widget w(1, 2, 3)` is decided by the first `1`.
/// * a **call** — `g()`, `g(a, b)`. A parameter's type is never called: `g()` as a *type* is not a thing, and
///   the shape [`parse_parameter_list`] would have to read it as — a parameter of type `g` with a default
///   argument — is what makes `Widget w(g(), h())` fail to parse at all.
/// * an **operator that cannot begin a type** — `+`, `-`, `!`, `~`, `%`, `|`, `==`, `<<`, `=`, and the rest of
///   the arithmetic, comparison and assignment families. A parameter's type is built from `*`, `&`, `&&`,
///   `::`, `<>`, `[]` and `()` and from nothing else, so no parameter's *type* begins with a `+`;
///   `Widget w(a + 1)` is decided by that.
///
/// The operators a type *does* contain are deliberately absent from that list, and `*` and `&` are why it has
/// to be a list rather than "any operator": `A(B * C)` is a multiplication and `A(B* C)` is a parameter named
/// `C` of type `B*`, with nothing but the symbol table to tell them apart. Leaving them out keeps such a
/// statement a call, which is the cheaper mistake — reading a value list as a parameter list leaves a variable
/// unbound and recoverable, while reading a parameter list as a value list swallows a function declaration and
/// everything after it.
fn the_arguments_look_like_values(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    let mut index = p.current_token_index() + 1;
    let mut depth = 0usize;
    // Are we at the position where an element begins? A list starts there, a `,` at this depth returns
    // there, and a nested bracket leaves it.
    let mut at_element_start = true;

    while index < p.token_count() {
        let kind = p.token_kind_at(index);

        if is_declaration_trivia(kind) {
            index += 1;
            continue;
        }

        match kind {
            // The list's own `)` at the outer depth ends it. Anything seen is settled by then.
            CppTokenKind::RightParen if depth == 0 => return false,
            CppTokenKind::RightParen
            | CppTokenKind::RightBracket
            | CppTokenKind::RightBrace => {
                depth = depth.saturating_sub(1);
                at_element_start = false;
            }
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                // A `(` in element position is a call, which is a value — not a type. Anywhere else it is
                // nested inside an element that is already under way.
                if at_element_start && kind == CppTokenKind::LeftParen {
                    return true;
                }
                depth += 1;
                at_element_start = false;
            }
            CppTokenKind::Comma if depth == 0 => at_element_start = true,
            CppTokenKind::IntegerLiteral
            | CppTokenKind::FloatingLiteral
            | CppTokenKind::StringLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::TrueKeyword
            | CppTokenKind::FalseKeyword
            | CppTokenKind::NullptrKeyword
                if at_element_start =>
            {
                return true
            }
            // A name, and then whatever follows it. The **next** token is what settles the element: a name
            // whose follower cannot be part of the declaration it introduces is a value, and no later token of
            // the element can change that.
            //
            // ```text
            // Inner(1)      a name that is called — a construction, not a type
            // a + 1         a name followed by an operator no type contains
            // a, b          a name followed by the list's own `,` — an element of its own
            // B* C          a name followed by `*`, which a type *does* contain: left alone
            // ```
            //
            // Reading the follower rather than the name is what keeps the last line out of the answer, and it
            // is why this cannot be decided by looking at the element's first token alone. `B*(C)` — a
            // multiplication and a parameter of type `B*` — stays a call, which is the cheaper mistake; see the
            // note on operators below.
            CppTokenKind::Identifier if at_element_start => {
                let follower = p.token_kind_at(next_significant_index(p, index));
                // The followers that keep this a *declaration*: another name (`B C`), a pointer or reference
                // (`B* c`), a qualified or templated continuation (`B::C`, `B<C>`), the `,` or `)` that ends a
                // nameless parameter (`void f(B)`), or a bracket or `=` that opens what a declarator carries.
                //
                // Everything else makes it a value, and the two that matter most are the `(` of a call and the
                // operators of an expression. The follower is the first token that is not trivia, so the space
                // in `int a (b)` is not read as the answer.
                let keeps_it_a_declaration = matches!(
                    follower,
                    CppTokenKind::Identifier
                        | CppTokenKind::Scope
                        | CppTokenKind::Less
                        | CppTokenKind::Star
                        | CppTokenKind::Ampersand
                        | CppTokenKind::LogicalAnd
                        | CppTokenKind::Comma
                        | CppTokenKind::RightParen
                        | CppTokenKind::LeftBracket
                        | CppTokenKind::Assign
                        | CppTokenKind::Ellipsis
                        | CppTokenKind::None
                );

                if !keeps_it_a_declaration {
                    return true;
                }
                at_element_start = false;
            }
            // An element that begins with an operator no type contains — `!flag`, `-x`, `~bits`.
            kind if at_element_start && !can_begin_a_type(kind) => return true,
            // A `*` or `&` in element position is ambiguous — `A(B* C)` is a parameter of type `B*` and
            // `A(*p)` is a dereference — and what separates them is whether a name follows. A pointer or
            // reference *declarator* has one, because `*` and `&` decorate the thing being declared:
            // `*`, `* const`, `*p`, `&r`. One with nothing but the end of the list after it is arithmetic.
            //
            // This is the same question [`can_begin_a_type`] answers for the start of an element, asked one
            // token later, and it is asked here rather than left to that function because the answer changes
            // with what follows: a bare `*` cannot begin a type.
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd
                if at_element_start =>
            {
                let mut after = next_significant_index(p, index);
                while matches!(
                    p.token_kind_at(after),
                    CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
                ) {
                    after = next_significant_index(p, after);
                }

                if !matches!(
                    p.token_kind_at(after),
                    CppTokenKind::Identifier
                        | CppTokenKind::Scope
                        | CppTokenKind::Star
                        | CppTokenKind::Ampersand
                        | CppTokenKind::LogicalAnd
                        | CppTokenKind::LeftParen
                ) {
                    return true;
                }
                at_element_start = false;
            }
            // The list ended without a `)`, or the statement did: there is no list to judge.
            CppTokenKind::Semicolon if depth == 0 => return false,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => at_element_start = false,
        }

        index += 1;
    }

    false
}

/// Can this token stand at the front of a type?
///
/// The set a parameter's type is built from, and nothing else: a specifier keyword, a name, a qualifier or
/// attribute, a `*`, `&`, `&&`, `...`, `::`, `(`, `[` or a template's `<`. Every token outside it — `+`, `-`,
/// `!`, `~`, `%`, `^`, `|`, `==`, `<<`, `=`, `?`, `.`, `->`, and the literals — can only be an operator or a
/// value, which is what [`the_arguments_look_like_values`] reads the answer from.
///
/// Written as "everything except the things a type *can* contain" rather than as the list of operators, because
/// the list of operators is what grows: a new spelling of a type that this does not know about would then be
/// misread as a value, while a new operator would only be misread as a type — the cheaper direction, and the
/// one this crate's other heuristics already choose.
///
/// # What is deliberately absent
///
/// `constexpr`, `mutable`, `virtual` and `explicit` are absent: they are storage and function specifiers, so
/// they cannot decorate a *parameter's* type, and what they really appear in — `Mutable x(1);` — is a variable
/// whose type is named `Mutable`. The specifier-sequence rule has already refused any of them followed by a
/// name in type position, so a `(` after one of them is an initialiser and this answer is the right one.
///
/// `~` is absent for the same kind of reason: it starts a destructor's *name*, and a parameter's type has no
/// name in it. `Widget w(~x)` is a value.
fn can_begin_a_type(kind: CppTokenKind) -> bool {
    is_type_specifier_keyword(kind)
        || matches!(
            kind,
            CppTokenKind::Identifier
                | CppTokenKind::Scope
                | CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::TypenameKeyword
                | CppTokenKind::Star
                | CppTokenKind::Ampersand
                | CppTokenKind::LogicalAnd
                | CppTokenKind::Ellipsis
                | CppTokenKind::LeftParen
                | CppTokenKind::LeftBracket
                | CppTokenKind::Less
        )
}

/// The index of the next significant token after `index`, or one past the end of the stream.
///
/// The token list is indexed, not peeked, in the scans above, so a question about the token *after* the one
/// being looked at has to step over trivia by index. `peek_token_kind_at` cannot answer it: it counts
/// significant tokens from the cursor, and the cursor is not where these scans are.
fn next_significant_index(p: &CppParser, index: usize) -> usize {
    let mut next = index + 1;
    while next < p.token_count() && is_declaration_trivia(p.token_kind_at(next)) {
        next += 1;
    }
    next
}

/// Do the parentheses at the cursor hold a list of *declarators* rather than a list of values?
///
/// The question the last heuristic in [`a_declaration_is_the_better_reading`] asks, and it is about shape rather
/// than about names. Two shapes answer yes, and they are the two things that can legitimately stand where a
/// direct-initialiser's argument goes:
///
/// * a **bare name** — `Max(a, b)`, two things already in scope being copied. A *value*, by contrast, is
///   written as a literal, a call, a member access or an operator, and none of those is a name;
/// * a **nested construction** — `Widget w(Inner(1))`, one value being built. A parameter list has no such
///   element: `Inner(1)` is not a type, and reading it as one is exactly how a constructor argument came out as
///   a parameter. It is accepted only where the declaration reading is already established, which is what the
///   `known_type_name` parameter carries — by then a constructed temporary is far likelier than a function whose
///   parameter is named `Inner` and defaulted from `1`.
///
/// Everything else disqualifies the list: a literal, a `::`, a `.`, a `->`, an operator, a bracket. Depth is
/// tracked so that a nested list terminates an element rather than silently continuing it.
///
/// The `)` must close the list. A scan that runs out of tokens answers no, because there is then no list.
fn the_arguments_look_like_declarators(p: &CppParser, known_type_name: bool) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    let mut index = p.current_token_index() + 1;
    let mut saw_one = false;
    let mut expect_an_element = true;
    let mut depth = 0usize;

    while index < p.token_count() {
        let kind = p.token_kind_at(index);

        if is_declaration_trivia(kind) {
            index += 1;
            continue;
        }

        match kind {
            // The list's own `)`, after one or more complete elements.
            CppTokenKind::RightParen => return depth == 0 && saw_one && !expect_an_element,
            CppTokenKind::LeftParen => {
                // A `(` that follows the name just read turns that name from a bare element into a
                // construction — but only where the declaration reading is already established, and only as one
                // level: the element `Inner(1)` is a value, whereas a call nested inside an element is not.
                if !known_type_name || expect_an_element || depth > 0 {
                    return false;
                }
                depth += 1;
            }
            CppTokenKind::Comma if depth == 0 => {
                if expect_an_element {
                    return false;
                }
                expect_an_element = true;
            }
            CppTokenKind::Identifier if depth == 0 => {
                // A second name with no comma between them is a type and its declarator, which this reading
                // has no element for.
                if !expect_an_element {
                    return false;
                }
                // A qualified name is a path to something rather than a name, so it is not a declarator.
                if p.peek_token_kind_at(1..2).as_slice() == [CppTokenKind::Scope] {
                    return false;
                }
                saw_one = true;
                expect_an_element = false;
            }
            // Inside a nested element: a value. Nothing there is this rule's business.
            _ if depth > 0 => {}
            // Anything else at the top level: a literal, an operator, a `::`, a `.`, an unmatched bracket.
            _ => return false,
        }

        index += 1;
    }

    false
}

/// Did the declaration this declarator belongs to lead with a name this file declared to be a type?
///
/// The companion to [`declarator_starts_with_a_type_keyword`], for the other way a type gets written: by name.
/// The name asked about is the **first** identifier of the declaration, which is what a declaration's type is —
/// `Widget` in `Widget w(1, 2);`.
fn declarator_starts_with_a_known_type_name(p: &CppParser) -> bool {
    p.declaration_type_name()
        .is_some_and(|name| p.is_a_known_type_name(name))
}

/// The name a type whose last segment starts at `from` would be looked up under.
///
/// The specifier loop that reaches this is called once per *segment* of a qualified name, because
/// [`parse_name`](super::types::parse_name) stops after each one and the loop walks the `::`. So the answer is
/// the **last** identifier of the whole thing: `std::string` is the type `string` seen through `std`, and a
/// lookup that started from `std` would be asking whether a *namespace* is a type.
///
/// The scan starts at the segment's own first token — which is the name, the caller having asked before parsing
/// it — and walks back over identifiers and the `::` between them, stopping at the first thing that is neither.
/// A nested name specifier is a chain of names, so reading the chain backwards finds its last link without
/// needing to know where the chain began. Only the identifier is kept, not the qualifiers: the qualifiers say
/// where to look the name up, which is a question for name resolution rather than for this table.
///
/// `""` when there is no such identifier, which the caller treats as "nothing to remember".
pub fn type_name_at(p: &CppParser, from: ParseAnchor) -> String {
    let mut index = p.current_token_index();

    loop {
        let kind = p.token_kind_at(index);
        if !is_declaration_trivia(kind) {
            if kind == CppTokenKind::Identifier {
                return p.token_text_at(index).to_string();
            }
            // Something that is neither a name nor the `::` between names: the chain ends here.
            if !matches!(kind, CppTokenKind::Scope) {
                return String::new();
            }
        }

        if index == from.token_index() {
            return String::new();
        }
        index -= 1;
    }
}

/// Did the declaration this declarator belongs to lead with a type **keyword**?
///
/// The question that separates `int a(1);` from `g(1, 2);`, and the only part of it that is answerable from the
/// tokens: a keyword type is written with a keyword, so a declaration beginning with one is a declaration,
/// while one beginning with a bare name is as likely to be the callee of a call.
///
/// Asked of the **first significant token of the declaration**, found by walking back over the tokens consumed
/// so far to the one that begins it. The walk stops at a `;`, `{` or `}` — no declaration contains one — which
/// is what keeps it inside this statement: without that, `void f() { g(1); }` would find the `void` of the
/// enclosing function and call the call a declaration.
///
/// Not asked of the event stream, which was the first attempt and does not work: the events reach back to the
/// beginning of the file, so the `BuiltinType` of an enclosing declaration is indistinguishable from this
/// one's by node kind alone.
fn declarator_starts_with_a_type_keyword(p: &CppParser) -> bool {
    let mut index = p.current_token_index();
    let mut first = None;

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        if matches!(
            kind,
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace
        ) {
            break;
        }
        if is_declaration_trivia(kind) {
            continue;
        }

        first = Some(kind);
    }

    // `auto` and `decltype` are deliberately left out of the answer even though they are in
    // [`super::types::is_type_specifier_keyword`]: both can begin an expression, so `auto(1)` is as readable as
    // a call as it is as a declaration. Of the two readings, the expression is the one to prefer — reading a
    // call as a declaration loses its callee *and* its arguments, while reading a declaration as a call merely
    // leaves a variable unbound.
    first.is_some_and(|kind| {
        super::types::is_type_specifier_keyword(kind)
            && !matches!(
                kind,
                CppTokenKind::AutoKeyword | CppTokenKind::DecltypeKeyword
            )
    })
}

/// Does a declarator begin where the cursor stands?
///
/// Used to refuse the empty reading of a parenthesized declarator. Only the openings that can start one are
/// accepted: the `(` of `g()` can start neither a declarator nor a name, so the reading is refused before the
/// parse is attempted rather than after it fails to say anything.
fn starts_a_declarator(p: &CppParser) -> bool {
    matches!(
        p.current_token(),
        CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            | CppTokenKind::Identifier
            | CppTokenKind::Scope
            | CppTokenKind::OperatorKeyword
            | CppTokenKind::Tilde
    )
}

/// Token kinds that are not part of a declaration's own text, for the walk above.
fn is_declaration_trivia(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace
            | CppTokenKind::Newline
            | CppTokenKind::LineContinuation
            | CppTokenKind::LineComment
            | CppTokenKind::BlockComment
            | CppTokenKind::None
    )
}

/// Can the `( ` at the cursor open an argument list rather than a parameter list?
///
/// True unless the parentheses are **empty**. `T x()` is the most vexing parse's other half — a function
/// declaration, not a variable — and it is already handled by the parameter-list reading, so the initializer
/// reading must not take it. Everything else is worth trying as an initializer, and the parameter reading has
/// already been tried and rewound by the time this is asked.
fn an_argument_list_follows(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::LeftParen
        && p.peek_token_kind_at(1..2).as_slice() != [CppTokenKind::RightParen]
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
        if p.current_token() == CppTokenKind::Dot || p.current_token() == CppTokenKind::LeftBracket
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

        // A pack expansion marker belonging to the parameter just parsed: `Args&&... args`, `Ts... rest`. It
        // comes *before* the name in the first spelling, so it is not something [`parse_parameter`] can attach
        // — by the time it returns, the cursor is past the name. The trailing `...` of an old-style variadic
        // function lands here too, which is why it is consumed on every path and not only before a comma.
        if p.current_token() == CppTokenKind::Ellipsis {
            p.bump();
        }

        match p.current_token() {
            CppTokenKind::Comma => {
                p.bump();
                // A trailing `...` after the last named parameter: `int f(int a, ...)`.
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
    //
    // The ellipsis is excluded because it is not one: in `Args&&... args` the marker comes between the type and
    // the name, so the name after it is what a declarator would have to be read from — and reading the `...`
    // as a declarator is what produced `expected a parameter list or an initializer` against these parameters.
    if !definitely_ends_a_type(p.current_token())
        && p.current_token() != CppTokenKind::Comma
        && p.current_token() != CppTokenKind::Ellipsis
        && let Err(err) = parse_declarator(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    // A pack expansion: `Args&&... args`. The marker belongs to the parameter it expands, so it is consumed
    // here; the name that follows it belongs to the same parameter and is read as one.
    if p.current_token() == CppTokenKind::Ellipsis {
        p.bump();

        if p.current_token() == CppTokenKind::Identifier
            && let Err(err) = parse_name(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }
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

    // A nested class is a type name inside this body and not outside it, so the table's scope follows the
    // brace — the same approximation a compound statement makes. The body depth is recorded for the same
    // region, because a `:` after a member declarator means something different in here than outside.
    p.enter_type_name_scope();
    p.enter_class_body();
    let result = parse_class_body_members(p);
    p.leave_class_body();
    p.leave_type_name_scope();
    result?;

    Ok(m.complete(p))
}

/// The members of a class body, up to its closing brace.
fn parse_class_body_members(p: &mut CppParser) -> ParseResult {
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

    Ok(CompleteMarker::empty())
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

/// Parse a destructor declaration: `~S();`, `~S() {}`, `virtual ~S() = default;`.
///
/// Entered from [`parse_declaration`] when the cursor is on the `~` of a declaration that has no specifiers —
/// which is the only way a destructor is ever written. The shape is a declaration of one init-declarator whose
/// declarator is the destructor name, so the nodes are the ordinary ones and a consumer finds the destructor
/// the same way it finds any other declaration.
///
/// `virtual ~S() = default;` and `inline ~S() {}` reach here *after* the specifier sequence has consumed
/// `virtual` or `inline`, because those are specifiers the sequence knows; the tilde after them is what stops
/// it. That is why this rule starts at the declarator rather than at a type.
fn parse_destructor_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declaration);

    let declarator_from = p.current_event_count();

    if let Err(err) = super::types::parse_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = finish_init_declarator(p, m, declarator_from) {
        p.close_marks_above(base);
        return Err(err);
    }

    // A destructor's body or its `;`, which is the same choice every function declarator gets at the end of
    // `parse_declaration` — and the reason the first attempt at this rule reported `expected ';'` against the
    // `}` of `~S() {}`: a declarator whose parameter list has been read is a *function*, and the brace after it
    // is a definition rather than a braced initializer.
    if p.current_token() == CppTokenKind::LeftBrace {
        if let Err(err) = parse_compound_stat(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(CompleteMarker::empty());
    }

    if let Err(err) = expect_semicolon(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(CompleteMarker::empty())
}

/// Is the qualified name at the cursor followed by an `=`, making this a `using` **alias**?
///
/// `using Base::method;` and `using Base::Alias = T;` begin the same way, and only the word at the end of the
/// name decides: an alias introduces a *type* and needs the type parser, while a using-declaration introduces a
/// name and does not. The scan walks the whole name — segments, template arguments and all — because the `=`
/// can be arbitrarily far away.
fn a_qualified_name_is_followed_by_an_equals(p: &CppParser) -> bool {
    for kind in p.peek_token_kind_at(0..96) {
        match kind {
            CppTokenKind::Assign => return true,
            // Anything that ends the statement or the name: there is no `=` in this one.
            CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::Eof
            | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
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

    // `using Alias = Type;` — an alias introduces a type name, and the parser needs it for the same reason a
    // `class` name is needed: `Alias a(1);` is a declaration and `f(1);` is a call.
    //
    // A using-*declaration* (`using ns::f;`) introduces whatever `f` already was, which this table cannot know,
    // so only the alias form is recorded. Recording the other would be recording a name as a type on the
    // strength of nothing.
    let alias_name = if p.current_token() == CppTokenKind::Identifier {
        Some(p.current_token_text().to_string())
    } else {
        None
    };

    // A using-*declaration*: `using ns::f;`, `using Base::method;`. What is introduced is the last segment of
    // a qualified name, and there is no type and no declarator — so the qualified name is read as **one** name
    // and the statement ends there.
    //
    // Read as a type-id instead, as it was, `Base::method` split into a type and a declarator: the type parser
    // walks `Base::` as a nested name specifier and then takes `method` for the declarator's name, and the
    // semicolon afterwards is a parameter list that never comes. The statement was reported as broken, and the
    // only spelling that worked was one qualifying a *type* — which is the alias form, not this one.
    if p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::Scope
        && !a_qualified_name_is_followed_by_an_equals(p)
    {
        let name = p.mark(CppSyntaxKind::NameExpr);
        p.bump(); // the first segment
        // The rest of the name, in a node of its own *inside* this one: `parse_name` opens a `NameExpr` for
        // what it reads, and a consumer asking a `NameExpr` for its text gets the text of its own tokens — so
        // wrapping the tail directly would report the first segment as the name and the qualifier as
        // everything else. `using ns::f;` introduces `f`, and this is what makes the tree say so.
        let rest = p.mark(CppSyntaxKind::NameExpr);
        if let Err(err) = super::types::parse_name(p) {
            rest.undo(p);
            name.undo(p);
            p.close_marks_above(base);
            return Err(err);
        }
        rest.complete(p);
        name.complete(p);

        if let Err(err) = expect_semicolon(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    if let Err(err) = parse_name(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if p.current_token() == CppTokenKind::Assign {
        p.bump();

        if let Some(name) = alias_name {
            p.declare_type_name(&name);
        }

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

    // `typedef int Integer;` makes `Integer` a type name, which is the whole point of the declaration — and the
    // name is what the declarator introduces, so it is read from there rather than from the specifiers.
    let defined_name = if p.current_token() == CppTokenKind::Identifier {
        Some(p.current_token_text().to_string())
    } else {
        None
    };

    if let Err(err) = super::types::parse_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    if let Some(name) = defined_name {
        p.declare_type_name(&name);
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
    if p.current_token() == CppTokenKind::Identifier && p.peek_next_token() == CppTokenKind::Assign
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

/// Parse the block of a linkage specification: `extern "C" { void f(); }`.
///
/// The linkage itself — the `extern` and its string — has already been consumed as a decl-specifier by
/// [`super::types::parse_decl_specifier_seq`], so what is left is the block. It is a namespace-shaped
/// construct rather than a statement block: everything inside is a declaration, and the names it introduces
/// are visible afterwards, which is why it is parsed by the declaration rule rather than
/// [`parse_stats_block`].
pub fn parse_linkage_block(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    expect_token(p, CppTokenKind::LeftBrace)?;

    // A linkage block is a scope, like a namespace body, so a type declared inside it is not a type outside.
    p.enter_type_name_scope();

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        let member_base = p.open_marks();
        let before = p.current_token_index();
        if parse_declaration(p).is_err() {
            p.close_marks_above(member_base);
            // Always advance: a declaration that consumed nothing would spin this loop forever.
            if p.current_token_index() == before {
                let error = p.mark(CppSyntaxKind::ErrorNode);
                p.bump();
                error.complete(p);
            }
        }
    }

    p.leave_type_name_scope();

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    let _ = base;
    Ok(m.complete(p))
}

/// Is the cursor on `extern` followed by a string literal — the head of a linkage specification?
///
/// Whether the declaration this appears in really is a specifier of one. `extern "C"` is the only form of
/// `extern` that is followed by a string, so the test is exact rather than a heuristic: an ordinary
/// `extern int x;` cannot reach it.
pub fn starts_a_linkage_specification(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::ExternKeyword
        && matches!(
            p.peek_token_kind_at(1..2).first(),
            Some(&CppTokenKind::StringLiteral)
        )
}

/// Parse a linkage specification: `extern "C" void f();` or `extern "C" { ... }`.
///
/// It is a declaration of its own rather than a specifier of the one after it, and that is what this rule
/// exists to express. Both grammars are legal C++, so the choice is about what the tree means:
///
/// * as a **specifier**, the declaration node would wrap a whole nested declaration, so `extern "C" void f();`
///   would be a `Declaration` whose `DeclSpecifierSeq` contains another `Declaration`, and a consumer walking
///   top-level declarations would find `f` one level deeper than everything else. The trailing `;` would then
///   have to be consumed by one of the two, and whichever one did not would report it as missing;
/// * as a **declaration**, the node says what the construct is — `extern "C"` applies to a declaration — and
///   the declaration it introduces is its child, so both readings a consumer wants ("what does this linkage
///   cover?" and "what is declared here?") are one level from the top.
fn parse_linkage_specification(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declaration);

    let specifiers = p.mark(CppSyntaxKind::DeclSpecifierSeq);
    let linkage = p.mark(CppSyntaxKind::ExternSpec);
    p.bump(); // `extern`
    p.bump(); // the string literal
    linkage.complete(p);
    specifiers.complete(p);

    if p.current_token() == CppTokenKind::LeftBrace {
        if let Err(err) = parse_linkage_block(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // One declaration shares the linkage, and its own `;` ends the whole construct — which is why nothing
    // here looks for a second one.
    if let Err(err) = parse_declaration(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}
