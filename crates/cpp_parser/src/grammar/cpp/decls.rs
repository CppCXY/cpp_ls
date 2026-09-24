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
    parser::{CompleteMarker, CppParser, MacroEvidence, Marker, MarkerEventContainer, ParseAnchor},
    parser_error::CppParseError,
    symbols::SymbolKind,
};

use super::{
    at_concept, at_requires, expect_contextual_keyword, expect_token,
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
///
/// Visible to the specifier sequence, which is the other rule that can meet a head: a declaration whose two
/// conditional branches each write one puts the second head in the middle of that sequence. See the note in
/// [`super::types::parse_decl_specifier_seq_with`].
pub(super) fn parse_template_head(p: &mut CppParser) -> ParseResult {
    // A template parameter list has the same `>`-closes-the-list property an argument list has:
    // `template <int N = 3>` must not read the `>` as "greater than". Restored on every exit path.
    let previous_depth = p.enter_template_arguments();
    let result = parse_template_head_inner(p, previous_depth);
    p.leave_template_arguments(previous_depth);
    result
}

fn parse_template_head_inner(p: &mut CppParser, outer_depth: usize) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateDecl);

    expect_token(p, CppTokenKind::TemplateKeyword)?;

    if let Err(err) = parse_template_parameter_list(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The parameter list is over, so the angles are closed and a `>` from here on is the greater-than operator
    // again. The clause below is where that shows: `template <typename T> requires (sizeof(T) > 1) void f();`
    // read the `>` as closing a template argument list that had already ended, and reported `expected )`.
    //
    // Restoring the *outer* depth rather than zero, in case this head is itself written inside template
    // arguments. The caller's own restore handles the way out.
    p.leave_template_arguments(outer_depth);

    // C++20: a template head may end in a **requires-clause**: `template <typename T> requires C<T> void f();`.
    // It comes after the parameter list and before whatever the head introduces, which is why this is the place
    // that can read it — the clause belongs to neither the parameters nor the declaration.
    //
    // `requires` is contextual, so the word alone decides nothing: `template <typename T> requires requires(T t)
    // { }` is a clause whose constraint is an expression, and a *name* spelled `requires` would be a parameter
    // list's worth of something else. Reading the clause is what tells them apart, which is why the clause rule
    // is entered and its failure tolerated rather than a lookahead being asked.
    if at_requires(p)
        && let Err(err) = parse_requires_clause(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a **concept declaration** (C++20): `concept Name = constraint;`.
///
/// Entered with the template head already consumed, so the cursor is on `concept`. The four parts are `concept`,
/// a name, `=`, and a constraint — and the constraint is read as an expression, because that is what it is.
///
/// The name is a plain name rather than a declarator: a concept introduces a *name* for a constraint, with no
/// type and no declarator around it, which is why this rule exists instead of the general one.
fn parse_concept_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ConceptDecl);

    expect_contextual_keyword(p, "concept")?;

    // The name. Optional in the grammar's own terms only for recovery: a concept without a name is broken, and
    // saying so once is better than consuming whatever follows as one.
    if p.current_token() == CppTokenKind::Identifier {
        if let Err(err) = parse_name(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    } else {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected a concept name",
            p.current_token_range(),
        ));
    }

    // The constraint, and then a requires-clause of its own may follow: `concept C = true && requires { … };`
    // is one expression, while `concept C = X requires Y;` is not valid — so there is no clause to read here.
    // What *is* read is the expression, and a requires-expression inside it is handled by the expression rule.
    //
    // Read with the braced-initialiser reading refused, for the same reason a requires-clause does: the
    // constraint ends at the `;`, and a `{` in it belongs to a requirement rather than to an initializer.
    //
    // The definition is read **once per branch** (`#if A = X; #else = Y; #endif`), which is how libstdc++ writes
    // `__is_signed_int128` and its neighbours; see [`parse_a_definition_per_branch`].
    let ended = parse_a_definition_per_branch(p, super::exprs::parse_constraint_expr);

    match ended {
        Err(err) => {
            p.close_marks_above(base);
            return Err(err);
        }
        // `true` means the branch's own `;` was the declaration's, so there is nothing left to ask for.
        Ok(ended) if !ended => {
            if let Err(err) = expect_semicolon(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        Ok(_) => {}
    }

    Ok(m.complete(p))
}

/// Is the `requires` at the cursor a **requires-clause** rather than an identifier?
///
/// `requires` is contextual, so the token alone decides nothing — `int requires = 1;` and `f(requires);` are both
/// valid programs, and a clause has to be told from them. The test is the one the type grammar uses for its own
/// ambiguities and for the same reason: **try the reading and see whether it consumes anything**.
///
/// ```text
/// requires C<T>;              a clause: a constraint follows
/// requires (C<T>);            a clause: a parenthesised constraint
/// requires requires { … }     a clause whose constraint is a requires-expression
/// requires = 1;               an identifier — `=` cannot begin a constraint, so nothing is consumed
/// requires;                   … and neither can `;`
/// requires(x);                a call — the parenthesis is consumed as a *constraint*, and what follows tells
/// ```
///
/// An earlier version of this asked what the token after `requires` was, from a list of "tokens that can begin an
/// expression". That list was wrong within minutes — it left out `&&`, so `requires C<T> && C2<T>` read the
/// clause as ending at `C<T>` — and it would have gone on being wrong for every operator added later. Trying the
/// parse has no such list to maintain, and it is the same bounded backtracking the declaration/expression
/// ambiguity already relies on.
///
/// Exposed to the expression grammar for the **nested requirement**: inside a requires-expression's body,
/// `requires C<T>;` is a clause while `requires;` is a simple requirement naming a variable called `requires`.
/// The same test separates them there, for the same reason.
pub(super) fn starts_a_requires_clause(p: &mut CppParser) -> bool {
    let checkpoint = p.checkpoint();
    let started_at = p.current_token_index();
    let parsed = parse_requires_clause(p);
    let consumed = parsed.is_ok() && p.current_token_index() > started_at;
    p.rollback(checkpoint);
    consumed
}

/// Parse a **requires-clause**: `requires` followed by a constraint expression.
///
/// Called from the four places the standard allows one, and shared by all of them because the clause is the same
/// construct in each — only its position differs:
///
/// ```text
/// template <typename T> requires C<T> void f();          after the template parameter list
/// template <typename T> void f(T t) requires C<T>;       after the declarator
/// template <typename T> struct S requires C<T> { };      after a class head
/// template <typename T> concept C = requires { f(); };   … and nested inside a requires-expression
/// ```
///
/// The constraint is read as an **expression**, which is what it is: `C<T>`, `C<T> && C2<T>`, `sizeof(T) > 4`,
/// `(C<T>)`, and a requires-expression are all expressions, and the operator rules already know how to read all
/// of them. Nothing here needs to know which of them it got.
///
/// It stops where an expression stops, which is what makes the clause safe to read in a position where a
/// declaration follows: `requires C<T> void f();` ends the constraint at `void`, because `void` cannot continue
/// an expression.
pub fn parse_requires_clause(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::RequiresClause);

    expect_contextual_keyword(p, "requires")?;

    // The constraint, read with the braced-initialiser reading refused: the `{` after it opens the *body* of
    // whatever the clause constrains. See [`super::exprs::parse_constraint_expr`].
    if let Err(err) = super::exprs::parse_constraint_expr(p) {
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
    definitely_ends_a_type(kind) || matches!(kind, CppTokenKind::Ellipsis | CppTokenKind::Greater)
}

/// The name this template parameter declares as a **type**, when it declares one.
///
/// Three spellings introduce a type parameter, and all three are asked of the **tokens in front** of the
/// parameter rather than of anything the parse produced:
///
/// ```text
/// typename T        class T        typename… Ts / class… Ts
/// ```
///
/// `template <…> class C` is a fourth, and it is asked after the inner head has been read — the name comes
/// *after* the head, which is the only reason this is called twice in [`parse_template_parameter`].
///
/// Everything else declares a **value** — `int N`, `auto N`, `size_t N` — and is deliberately not a type name:
/// `N[4]` is a subscript of an array, not an array of `N`.
fn a_type_parameter_name(p: &CppParser) -> Option<String> {
    if !matches!(
        p.current_token(),
        CppTokenKind::TypenameKeyword | CppTokenKind::ClassKeyword
    ) {
        return None;
    }

    // `typename… Ts` / `class… Ts`: the name follows the pack marker, which follows the keyword.
    if p.peek_next_token() == CppTokenKind::Ellipsis {
        let name = p.peek_token_text_at(2);
        return (!name.is_empty()).then(|| name.to_string());
    }

    if p.peek_next_token() != CppTokenKind::Identifier {
        return None;
    }

    Some(p.peek_token_text_at(1).to_string())
}

/// Parse one template parameter: `typename T`, `class C`, `int N`, `template <...> class T`,
/// `T...`, or a constrained parameter `C T`.
fn parse_template_parameter(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateParameter);

    // Which parameters declare a **type**, recorded before anything consumes them: the specifier sequence reads
    // `typename T` as one specifier, so by the time it returns the name is behind the cursor and the spelling
    // that said "this is a type" is the only thing that could have told us. `int N` and `auto N` declare values
    // and are deliberately not recorded — `N[4]` is not an array of anything.
    if let Some(name) = a_type_parameter_name(p) {
        p.declare_template_parameter(&name);
    }

    // A template template parameter: `template <typename> class C`.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && let Err(err) = parse_template_head(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    // The name of a **template template** parameter comes after its head: `template <…> class C`. The head has
    // just been read, so this is the first moment the `class C` half can be seen, and the spelling is the same
    // one the check above looks for.
    if let Some(name) = a_type_parameter_name(p) {
        p.declare_template_parameter(&name);
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

            // Read **below the comma operator**: the comma after a default argument separates the next
            // parameter. `template <typename T, int N = 3, typename... Rest>` is three parameters, and a reader
            // that took the comma swallowed the third — which is how this rule was found.
            if let Err(err) = super::exprs::parse_assignment_expr(p) {
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
    // A declaration is the one place a bare template-id may stand where a *name* belongs, and only when the
    // declaration is an **explicit instantiation**: `extern template void f<int>(int);` names the instantiation
    // it asks for, and that name is a template-id. Everywhere else `C<T> x;` gives the arguments to the type.
    //
    // Cleared on entry and restored on the way out, on every path including the error ones: a flag that
    // outlived its declaration would let the *next* one read a bare template-id as a name, which is the silent
    // wrong tree the rule exists to prevent. See [`crate::grammar::cpp::types::a_bare_template_id_is_here`].
    let outer = p.a_template_id_may_be_the_name();
    p.set_a_template_id_may_be_the_name(false);

    // A **template parameter** is a name declared as a type, and it is scoped to the declaration its head
    // introduces — so this is where that scope ends. The count is saved rather than the list, and cut back
    // rather than cleared, so a nested declaration (a member of a class template) keeps the parameters of the
    // head around it: what *it* saved already includes them.
    let parameters_before = p.template_parameter_count();

    let result = parse_declaration_here(p);

    p.truncate_template_parameters(parameters_before);
    p.set_a_template_id_may_be_the_name(outer);
    result
}

fn parse_declaration_here(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let checkpoint = p.checkpoint();

    // Declarations whose shape is not `specifiers declarators ;` get their own rules, dispatched
    // first because the general rule would read their keyword as a type specifier and then report
    // nonsense about everything after it.
    match p.current_token() {
        CppTokenKind::NamespaceKeyword => return parse_namespace_declaration(p),
        CppTokenKind::TypedefKeyword => return parse_typedef_declaration(p),
        CppTokenKind::StaticAssertKeyword => return parse_static_assert(p),

        // `inline namespace v1 { ... }` — an **inline namespace**, which is a namespace whose members are also
        // members of the enclosing one.
        //
        // Dispatched here because `inline` is dispatched nowhere and `namespace` is not a type: the specifier
        // sequence claims `inline`, stops at `namespace`, and the declaration is refused. The keyword is
        // consumed *before* delegating so that the namespace rule starts where it expects to — and it is
        // consumed rather than skipped, so a consumer reading the `NamespaceDecl`'s own tokens can still see
        // that the namespace is inline.
        CppTokenKind::InlineKeyword if p.peek_next_token() == CppTokenKind::NamespaceKeyword => {
            p.bump(); // `inline`
            return parse_namespace_declaration(p);
        }

        // `extern template struct S<int>;` — an **explicit instantiation declaration** — is consumed *inside*
        // the declaration node, below, rather than here: a token is emitted at the moment it is consumed, so
        // consuming it before the marker existed would put `extern template` beside the declaration instead of
        // in it, and "is this declaration an instantiation rather than a definition?" is answered by reading the
        // declaration's own tokens.

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

        // A **conversion operator** — `operator int();`, `operator std::string() const;`.
        //
        // Dispatched here for the same reason as the destructor: the name is written first, so no type
        // precedes it and the specifier sequence has nothing to read. It used to be dispatched nowhere at
        // all, and the result was silent: `operator` became an error node, `int` a declaration of a variable
        // named `int`, and the `()` another error — with no message reported.
        //
        // Safe to claim unconditionally, because no *other* declaration begins with `operator`: an overloaded
        // operator's return type comes first (`Ops operator+(const Ops&)`), so a declaration whose first token
        // is `operator` can only be a conversion.
        CppTokenKind::OperatorKeyword => return parse_conversion_operator_declaration(p),

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

    // `extern template void f<int>(int);` — an **explicit instantiation declaration**, which says the
    // instantiation is defined in another translation unit.
    //
    // Not a linkage specification, which is the other thing `extern` introduces: that one is followed by a
    // string literal. This one is followed by `template`, and the declaration after it is an ordinary
    // declaration whose *name* is a template-id — so consuming both keywords and letting the general rule below
    // run is the whole fix. Without it the specifier sequence claims `extern`, stops at `template`, and reports
    // `expected ;`.
    //
    // Consumed **inside** the declaration node rather than at the dispatch above: a token is emitted at the moment
    // it is consumed, so consuming it before the marker existed would put `extern template` beside the
    // declaration instead of in it, and "is this declaration an instantiation rather than a definition?" is
    // answered by reading the declaration's own tokens.
    //
    // The two words together are unambiguous — nothing else in C++ spells `extern template` — so no third
    // condition is asked. There used to be one, and it was wrong in a way that is easy to miss: it required a
    // *keyword* type after the `template`, so `extern template MyType f<int>(int);` — legal C++, with a return
    // type the file names itself — was refused while `extern template void f<int>(int);` was read.
    if p.current_token() == CppTokenKind::ExternKeyword
        && p.peek_next_token() == CppTokenKind::TemplateKeyword
    {
        p.bump(); // `extern`
        p.bump(); // `template`

        // What follows names the instantiation, and for a function that name is a **template-id**:
        // `extern template void f<int>(int);`. Recorded for the declarator rule, which otherwise refuses one —
        // correctly, since a declarator's name may not have template arguments in any other declaration.
        p.set_a_template_id_may_be_the_name(true);
    }

    // The same construct **without** `extern`: `template void f<int>(int);`, `template class C<int>;`,
    // `template int v<int>;`.
    //
    // The standard writes the two spellings as one production — `explicit-instantiation: extern(opt) template
    // declaration` — and the only visible difference is the keyword that may be absent. What made this the harder
    // half is that a bare `template` is otherwise the start of a **template head**, which requires a `<`: the
    // head rule was entered, failed on the type specifier that followed, and took the whole declaration down with
    // it. So the `<` is what tells the two apart, and it is asked here, where both readings are still available.
    //
    // A `template` followed by `<` is a head and is left to the rule below; a `template` followed by anything else
    // cannot be one, because a parameter list is not optional.
    if p.current_token() == CppTokenKind::TemplateKeyword
        && p.peek_next_token() != CppTokenKind::Less
    {
        p.bump(); // `template`
        p.set_a_template_id_may_be_the_name(true);
    }

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
    //
    // # Why it is a loop, and what the file writes
    //
    // Because a conditional **alternation of heads** is a real spelling, and `bits/basic_string.h` writes one —
    // two constructors, one per standard level, each with its own head, both inside one declaration:
    //
    // ```cpp
    // #if __cplusplus >= 201103L
    //   template<typename _InputIterator,
    //            typename = std::_RequireInputIter<_InputIterator>>
    // #else
    //   template<typename _InputIterator>
    // #endif
    //   _GLIBCXX20_CONSTEXPR
    //   basic_string(_InputIterator __beg, _InputIterator __end, const _Alloc& __a = _Alloc())
    // ```
    //
    // Reading **one** head and then one directive is what left that member unread: the second `template` reached
    // the specifier sequence, which has no reading for it, so the declaration failed, the whole attempt was rolled
    // back to the first `template`, and the class body's recovery then wrapped the head one token at a time. The
    // member was not a member, and every declaration after it was read as a child of the rubble. Both halves are
    // needed and both are the same seam: a directive between a head and what it heads, or between two of them.
    let mut seen_a_template_head = false;
    loop {
        if p.current_token() == CppTokenKind::TemplateKeyword {
            // Whatever head it is — empty or not — the declaration it introduces is one of the three that may be
            // **named by a template-id**, because each of them has to say *which* template it is about:
            //
            // ```text
            // template <> void f<int>(int);              an explicit specialization: the head is empty
            // template <class T> bool v<T*> = true;      a partial specialization: it is not
            // ```
            //
            // The empty spelling used to be the only one that set this, and the non-empty one was left to the rule
            // that refuses a bare template-id in a declarator's name — a rule written for the *type/name* ambiguity
            // (`C<T> x;` gives the arguments to the type), which has nothing to resolve here: a variable template's
            // partial specialization writes its arguments in the name position and has nowhere else to put them. It
            // cost `bits/stl_pair.h` (`__is_tuple_v<tuple<_Ts...>>`), `concepts` (`__destructible_impl<_Tp>`) and
            // `bits/functional_hash.h` their first error each.
            //
            // Setting it for *every* templated declaration costs nothing: the flag is only consulted at a declarator's
            // name, and no other declaration a head can introduce writes a template-id there — `template <class T>
            // C<T> x;` gives the arguments to the type, which the specifier sequence has already taken.
            p.set_a_template_id_may_be_the_name(true);

            if let Err(err) = parse_template_head(p) {
                p.rollback(checkpoint);
                return Err(err);
            }
            seen_a_template_head = true;
            continue;
        }

        // A directive between the head and what it heads — the `#endif` half of the shape above, and of
        // `#if __cpp_deduction_guides … template<…> #endif`. Read as the node it is, and go round again: the next
        // token is either another head or the declaration itself, and both are answers this loop already has.
        if seen_a_template_head && p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                p.rollback(checkpoint);
                return Err(err);
            }
            continue;
        }

        // **A clause that a directive pushed away from the head.** The head rule reads a requires-clause written
        // directly after its parameter list, but libstdc++ puts one *inside a conditional* — and then the clause
        // arrives here, with the directive already read:
        //
        // ```cpp
        // template<typename _Tp, typename _Up>
        // #if __cpp_concepts                                            // bits/alloc_traits.h:72
        //   requires requires { typename _Tp::template rebind<_Up>::other; }
        //   struct __rebind<_Tp, _Up>
        // #else
        //   struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>
        // #endif
        //   { using type = …; };
        // ```
        //
        // `requires` is contextual, so the arm is a spelling test with the shape test beside it — the same pair the
        // declarator's clause arm uses (`starts_a_requires_clause` tries the clause and reports whether it consumed
        // anything). Read as a clause of the declaration rather than of the head, which is the only place left for
        // it once a directive stands between: the `TemplateDecl` has been completed by then.
        if seen_a_template_head && at_requires(p) && starts_a_requires_clause(p) {
            if let Err(err) = parse_requires_clause(p) {
                p.rollback(checkpoint);
                return Err(err);
            }
            continue;
        }

        break;
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

    // A **concept declaration** (C++20): `template <typename T> concept C = constraint;`.
    //
    // Dispatched here, after the head, because that is the shape of the construct: a concept always has one. It
    // is also why it is dispatched beside `using` and `typedef` rather than at the top of this function — at the
    // top the cursor is on the `template` keyword.
    //
    // A concept has no return type and no declarators — it is `concept`, a name, `=`, and a constraint — so the
    // general rule below, which begins with a specifier sequence, has nothing to start from.
    //
    // **The head is required, and that is what keeps the word usable as a name.** `concept` is contextual, so the
    // spelling alone says nothing, and a bare `concept = 2;` is an assignment to a variable of that name — asking
    // this rule to read it produced `expected a concept name` against the `=`. A concept definition always has a
    // template head, so the head is the condition that tells the two apart, and it is one the parser already
    // knows because it just read one.
    if seen_a_template_head && at_concept(p) {
        if let Err(err) = parse_concept_declaration(p) {
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

    // Attributes written *between* the template head and the declaration it wraps:
    // `template <typename T> [[nodiscard]] T p();`. That position belongs to neither the head nor the
    // specifier sequence — the head has closed by the time the `[[` appears, and the sequence has not started
    // — so it was read by the *declarator*, which reported `expected ';'` against the attribute.
    //
    // Only when a head was actually consumed: an attribute at the start of an ordinary declaration is a
    // specifier and the sequence below owns it.
    if seen_a_template_head && let Err(err) = super::types::parse_attribute_specifiers(p) {
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
    //
    // The second half of the condition is the case where it consumed the head but **not** the body, because a
    // directive stood between them:
    //
    // ```cpp
    // template<typename _Tp, typename _Up>                                  // bits/alloc_traits.h:72
    //   requires requires { typename _Tp::template rebind<_Up>::other; }
    // #if __cpp_concepts
    //   struct __rebind<_Tp, _Up>                     // this branch's head has no body …
    // #else
    //   struct __rebind<_Tp, _Up, __void_t<…>>
    // #endif
    //   { using type = …; };                          // … and the body after `#endif` is shared by both
    // ```
    //
    // `declaration_opens_a_body` cannot see it: it is answered from the tokens **after the cursor** (the
    // declaration's leading keywords are behind it by now, and all that is left in front is the brace). So the
    // question is asked of the specifiers' own events instead — did they write a class-like head? A `{` after one
    // is that head's body and nothing else, since a variable whose type is a class definition has already had its
    // initializer read by the declarator rule.
    if p.current_token() == CppTokenKind::LeftBrace
        && (declaration_opens_a_body(p) || declaration_wrote_a_class_head(p, specifiers_from))
    {
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

    // An **old-style (K&R) parameter list ends the declaration as well**: the `;` that closed its last parameter
    // declaration closed *this* declaration too, so there is no second one to expect.
    //
    // ```c
    // #if defined(__CLASSIC_C__)
    // int main(argc, argv)
    // int argc;
    // char *argv[];
    // #else
    // int main(int argc, char *argv[])
    // #endif
    // { … }
    // ```
    //
    // That is the shape the CMake compiler-id probe is written in, and it is the reason the arm above exists: the
    // head and the body sit in *different branches* of one conditional, so the declaration that carries the
    // old-style list has neither a body of its own nor a `;` left to give — `char *argv[];` spent it. Asking again
    // reported `expected ';'` against the `#else`, and a single diagnostic against a *preprocessor* line took the
    // whole definition with it.
    //
    // Nothing but a body can follow a function declarator, so this costs no reading: a declaration that follows is
    // one the old-style list has already swallowed, and the tokens after the cursor belong to whichever rule owns
    // them — a body (read above), a directive, or a statement that reports on its own terms rather than through a
    // declaration that was never wrong.
    if p.events_contain_any(specifiers_from, &[CppSyntaxKind::OldStyleParameterList]) {
        return Ok(m.complete(p));
    }

    // **A declaration that gave up but keeps its tokens** closes what it opened *with* the end events
    // ([`MarkerEventContainer::end_marks_to`]). `close_marks_above` would detach them without an event, and an
    // unpaired `NodeStart` is balanced by the tree builder at the end of the stream — so the declaration that
    // failed here would swallow every token written after it. This is the path `int x = 1` (with no `;`) and
    // `__glibcxx_class_requires(_Tp, Concept)` take inside a class body, and it is why one such member took the
    // rest of `std::vector`'s class with it. The two paths above this one roll back instead, which is the other
    // correct answer: no events, nothing to pair.
    if let Err(err) = expect_semicolon(p) {
        p.end_marks_to(base);
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

/// Did the specifier sequence just parsed write a **class-like head** — with or without its body?
///
/// The companion of [`declaration_defined_a_class`], and the difference is the case this exists for: a head whose
/// body is on the *other* side of a directive, so the specifiers hold the head alone (see the note at the call
/// site). `class Foo;` also answers yes, and that is right — a brace after it is an error either way, and letting
/// the body rule report it is more informative than reading the brace as an initializer of nothing.
fn declaration_wrote_a_class_head(p: &CppParser, specifiers_from: usize) -> bool {
    p.events_contain_any(
        specifiers_from,
        &[
            CppSyntaxKind::ClassDef,
            CppSyntaxKind::StructDef,
            CppSyntaxKind::UnionDef,
            CppSyntaxKind::EnumDef,
            CppSyntaxKind::EnumClassDef,
        ],
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

    // Attributes written **after the declarator**: `int x [[maybe_unused]] = 1;`,
    // `void f() [[noreturn]];`, `[[nodiscard]] int g() [[deprecated]] { return 1; }`.
    //
    // This is the position C++ calls "after the declarator-id and before the initializer", and it is the one
    // that was missing: the specifier sequence reads the attributes written *before* the type, and the
    // class-head rule reads the ones written after a class's name, but a `[[` here reached the initializer
    // rule, which has no notion of an attribute.
    //
    // Consumed before the `declarator_is_function` answer is used rather than after, because the token after
    // the attributes is what decides — `void f() [[noreturn]] { }` has a body, and the brace is one token
    // further along than the match below would look.
    if let Err(err) = super::types::parse_attribute_specifiers(p) {
        m.undo(p);
        return Err(err);
    }

    // A **macro** in the same position: `__atomic_flag_data_type _M_i _GLIBCXX20_INIT({});`. Read after the
    // attributes and before the initializer question below, because the token *after* it is what decides whether
    // there is an initializer at all — `int x MACRO = 1;` has one and `int x MACRO;` does not. See
    // [`eat_a_macro_suffix`] for why a name here costs nothing.
    while eat_a_macro_suffix(p) {}

    // **An initializer needs something to initialise.** Without this the declaration reading accepts a
    // declarator that named nothing and is followed by `=`, and what comes out is a *silent wrong tree*: `x = 1;`
    // became `Declaration(DeclSpecifierSeq(x), InitDeclarator(=, Initializer(1)))` — no error, no `ErrorNode`,
    // no `MissingNode`, and a shape a consumer reads as a declaration of a variable named by nothing. Every
    // assignment whose left-hand side this file has not seen a type for was read that way, which is most
    // assignments in a body.
    //
    // Failing here is what lets the statement rule do its job: the declaration reading is rewound and the
    // expression reading — the one that owns an assignment — gets the tokens.
    //
    // The condition is asked *after* the attributes are consumed, because an attribute is not a name and must
    // not be mistaken for one; `a_name_was_parsed` reads the declarator's own events, which is the same
    // question [`a_declaration_is_the_better_reading`] asks about `Widget w(1)` versus `g(1)`.
    if an_initializer_needs_a_name(p, declarator_from) {
        m.undo(p);
        return Err(CppParseError::syntax_error_from(
            "expected a declarator name",
            p.current_token_range(),
        ));
    }

    // **Directives between a function's head and its body.** A conditional head is a real spelling — the two
    // variants of a signature, one per platform:
    //
    // ```c
    // #if defined(_WIN32)
    // void f(void)
    // #else
    // void f()
    // #endif
    // { … }
    // ```
    //
    // A `#` here cannot be anything else: a declarator is followed by a body, a `;`, an initializer — or a
    // directive. Read as the node it is, and the match below is then asked about the token that really follows.
    // The same argument as the two places `docs/grammar-gaps.md` records for B23; a `#` anywhere else in an
    // expression is still an error.
    //
    // # Why this alternates with the macro suffixes
    //
    // Because a conditional can decide **which** suffix is written, and libstdc++ writes its copy-on-write
    // constructor exactly that way (`bits/cow_string.h:515`):
    //
    // ```cpp
    // basic_string()
    // #if _GLIBCXX_FULLY_DYNAMIC_STRING == 0
    //   _GLIBCXX_NOEXCEPT                     // a macro suffix, in one branch only
    // #endif
    // #if __cpp_concepts && __glibcxx_type_trait_variable_templates
    //   requires is_default_constructible_v<_Alloc>
    // #endif
    // #if _GLIBCXX_FULLY_DYNAMIC_STRING == 0
    //   : _M_dataplus(…)                      // …and the initializer list, one per branch
    // #else
    //   : _M_dataplus(…)
    // #endif
    //   { }
    // ```
    //
    // With the two loops written one after the other, the macro suffix after `#endif` reached the match below,
    // which has no reading for a bare identifier between a declarator and its body: the declaration failed, and
    // the recovery then took the constructor's `{ }` for the class's closing brace — so `class basic_string`
    // ended 3400 lines early and every member after it was read at file scope. Alternating is the whole fix: a
    // directive may be followed by a macro, and a macro by a directive.
    loop {
        let mut consumed = false;
        while declarator_is_function && p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                m.undo(p);
                return Err(err);
            }
            consumed = true;
        }
        consumed |= eat_a_macro_suffix(p);
        if !consumed {
            break;
        }
    }

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
        // **A declarator takes one initializer**, and this arm is the shape where a second one is written.
        //
        // The suffix reader can already have read one — `T x(y)` is a direct-initialisation, and which of the two
        // readings `(y)` gets is the preference [`parse_function_suffix_or_initializer`] decides. When it reads it
        // as an initializer, the declarator is **not** a function, and the `{` that follows is a *second*
        // initializer: no declaration has two, and the tree that came out was well formed, lossless and silent —
        //
        // ```cpp
        // struct Base {
        //   _GLIBCXX20_CONSTEXPR void f(size_type) { }   // ← bits/stl_vector.h:192, in `struct _Grow`
        //   int after;
        // };
        // ```
        //
        // read as a variable `f` of type `_GLIBCXX20_CONSTEXPR void` initialised with `size_type` **and then**
        // with `{ }`, after which the declaration had no `;` to end on and `int after;` became its child. That one
        // member took `std::_Vector_base`'s whole class body with it — the facts stopped at the `_Vector_impl`
        // declaration, and `std::vector` had no members at all.
        //
        // Failing here is what lets the recovery do its job, and the recovery is unusually good at this one: the
        // member loop wraps *one* token (`_GLIBCXX20_CONSTEXPR`) in an error node, tries again at `void`, and the
        // parameter reading — which is the right one for `void f(size_type)` — wins with nothing in front of it.
        // The member is read correctly and the ones after it are members, which is the same "advance by one token"
        // property that makes the error-node recovery worth keeping (see the note on `at_a_macro_member`).
        CppTokenKind::LeftBrace
            if !declarator_is_function
                && p.events_contain_any(declarator_from, &[CppSyntaxKind::Initializer]) =>
        {
            m.undo(p);
            return Err(CppParseError::syntax_error_from(
                "a declarator takes only one initializer",
                p.current_token_range(),
            ));
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
            parse_further_member_initializer_lists(p)?;
        }
        // A **requires-clause** after the declarator (C++20): `void f(T t) requires C<T>;`.
        //
        // Read here because this is where a declarator ends — and it is restricted to a **function** declarator,
        // which is the flag the neighbouring arms already consult. The standard requires more than that
        // ([dcl.decl.general]/5: the clause belongs to a *templated* function), but a function is the part of it
        // that is visible in the tokens, and it is the part that matters: nothing but a body, a `;` or a
        // constructor's `:` can follow a declarator whose clause has been read.
        //
        // **The clause is not the last suffix**, which is the part this arm got wrong for a long time:
        //
        // ```cpp
        // basic_string()                                    // bits/basic_string.h:585
        // _GLIBCXX_NOEXCEPT_IF(is_nothrow_default_constructible<_Alloc>::value)
        // #if __cpp_concepts && __glibcxx_type_trait_variable_templates
        // requires is_default_constructible_v<_Alloc>
        // #endif
        // : _M_dataplus(_M_local_data())
        // { _M_init_local_buf(); _M_set_length(0); }
        // ```
        //
        // A clause is one of the things written *after* a declarator, and the member initializer list is another
        // one that may follow it, with a directive in between — which is what this file does. Returning from the
        // match at the clause left the `:` to the class body, where the only reading available is "not a member":
        // it became an `ErrorNode`, `_M_dataplus(_M_local_data())` became a **declaration** of its own, and every
        // member written after it — the whole public interface of `std::basic_string` — was read as a child of one
        // bogus declaration instead of as a member of the class. Lossless, well formed, and silent: the class
        // body's recovery wraps an unexpected token and moves on, so no error was reported at any of it.
        //
        // The two steps below belong to *this* suffix rather than to the arms above: a `:` with no clause in front
        // of it is already the arm above's, and a directive *before* a clause is the loop before the match's. When
        // a third suffix turns out to be able to follow a clause, this becomes a loop over suffixes rather than a
        // match — see the maintenance conventions on the third occurrence of a rule.
        //
        // **`struct S requires C<T> { };` is not read, and that is deliberate.** The grammar gives a class head
        // no clause at all, and reading one used to detach the class body: the `{ }` that follows the constraint
        // was left for the statement rule, so the tree came out as a *struct declaration* followed by a
        // `CompoundStat` at file scope — well formed, lossless, no diagnostic, and with the class's members
        // belonging to nothing. Reporting `expected ;` against the `requires` is both what the standard says and
        // the only reading that keeps the body where it belongs.
        //
        // `requires` is contextual, so the arm is a *spelling* test with the shape test beside it: the clause is
        // only taken when what follows can begin a constraint. `requires` used as an identifier (`int requires =
        // 1;`) is a declarator **name**, and it reaches this match as the token that ends the declarator rather
        // than beginning a clause — `starts_a_requires_clause` tries the clause and reports that it consumed
        // nothing, which is the same test the class-head refusal above relies on.
        CppTokenKind::Identifier
            if at_requires(p) && p.last_declarator_is_function() && starts_a_requires_clause(p) =>
        {
            // Asked **before** the clause is parsed, because the constraint is an expression: an expression may
            // contain a declarator of its own (`decltype(…)`, a lambda's parameter list) and parsing one resets
            // the flag this needs to read afterwards.
            let constrains_a_function = p.last_declarator_is_function();
            parse_requires_clause(p)?;

            // A directive between the clause and what follows it, for the reason the loop before the match gives.
            while p.current_token() == CppTokenKind::Hash {
                if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                    m.undo(p);
                    return Err(err);
                }
            }

            // …and the member initializer list a constrained **constructor** may still write after its clause:
            // `S(U u) requires C<U> : a(u) { }`. Without this the `:` is nobody's token; with it, the caller sees
            // the body's `{` where the grammar says it should be.
            if constrains_a_function && p.current_token() == CppTokenKind::Colon {
                parse_member_initializer_list(p)?;
                // …and the other branches' lists, for the same reason this arm exists at all: a conditional can
                // put one per branch, with a directive in between.
                parse_further_member_initializer_lists(p)?;
            }
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
        CppTokenKind::Colon if p.is_at_class_member_level() => {
            let width = p.mark(CppSyntaxKind::Initializer);
            p.bump(); // `:`

            // The width is a constant-expression, and it is optional in the grammar's own terms only because a
            // nameless bit-field is written `int : 0` — the `:` is never followed by a `;`.
            //
            // Read **below the comma operator**, because a bit-field list is comma-separated:
            // `unsigned flags : 1, spare : 7;` is two fields, and a width reader that took the comma would
            // swallow the second one. This is one of the rules the maintenance convention is about — a rule that
            // spells a comma itself has to opt out of the operator that spells commas.
            if let Err(err) = super::exprs::parse_assignment_expr(p) {
                width.undo(p);
                return Err(err);
            }
            width.complete(p);
        }
        // An **old-style (K&R) parameter list**: the parameters are declared after the parenthesis rather than
        // inside it.
        //
        // ```c
        // int main(argc, argv)
        //     int argc;
        //     char *argv[];
        // { … }
        // ```
        //
        // The parenthesis has already been read, and read as a *parameter list* — `argc` is a perfectly good
        // parameter type, and nothing in those tokens says it is a name instead. What identifies the old style is
        // what follows: a declaration, where a modern function has its body or its `;`. The declarations are read
        // by the ordinary declaration rule and kept in a list node of their own, so a consumer gets the
        // parameters' types without re-reading tokens.
        //
        // Obsolete in C++ and deprecated in C, but ordinary in C from before 1989 — and this parser is asked to
        // read C. Nothing else can follow a function declarator with a declaration, so the reading costs no
        // modern spelling: `void f() int x;` is not a program in any language, and the tokens say what was meant.
        _ if declarator_is_function && starts_an_old_style_parameter_list(p) => {
            let list = p.mark(CppSyntaxKind::OldStyleParameterList);

            loop {
                let before = p.current_token_index();
                let checkpoint = p.checkpoint();
                match parse_declaration(p) {
                    // A declaration that consumed nothing would spin this loop; one that failed is not an
                    // old-style parameter declaration, which ends the list rather than the declaration.
                    Ok(_) if p.current_token_index() > before => {}
                    _ => {
                        p.rollback(checkpoint);
                        break;
                    }
                }

                if !starts_an_old_style_parameter_list(p) {
                    break;
                }
            }

            list.complete(p);
        }
        _ => {}
    }

    Ok(m.complete(p))
}

/// Does an old-style (K&R) parameter declaration list start at the cursor?
///
/// The question is "can a declaration start here?", and a declaration starts with a **type**: a type keyword
/// (`int argc;`, `char *argv[];`), a class keyword (`struct S x;`), the other specifiers that only a declaration
/// begins — or a **name this file knows to be a type**, because `size_t argc;` is as ordinary in old C as
/// `int argc;` is.
///
/// [`starts_declaration`] is consulted for the specifiers and cannot answer on its own: it holds the anchors a
/// *statement* needs to tell a declaration from an expression, and a plain type keyword is deliberately not one of
/// them — `int x;` is reached by trying the declaration reading, not by an anchor. A `[` or a `*` reaching here is
/// how the two lists differ, which is exactly what the question needs.
///
/// Asked before the declarations are read rather than after they fail, so that a token which cannot begin a
/// declaration — the body's `{`, the `;` of a declaration, a `throw()` specification — is left to the arms that
/// do own it.
fn starts_an_old_style_parameter_list(p: &mut CppParser) -> bool {
    if matches!(
        p.current_token(),
        CppTokenKind::LeftBrace
            | CppTokenKind::Semicolon
            | CppTokenKind::Eof
            | CppTokenKind::None
            | CppTokenKind::Hash
    ) {
        return false;
    }

    super::types::is_type_specifier_keyword(p.current_token())
        || starts_declaration(p)
        || (p.current_token() == CppTokenKind::Identifier
            && p.is_a_known_type_name(p.current_token_text()))
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

    // An **element** of whatever list holds this clause, so it is read below the comma operator — the commas
    // between elements are the list's. `int v[] = {1, 2}` is two elements for exactly this reason.
    super::exprs::parse_assignment_expr(p)
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
pub fn parse_function_suffix_or_initializer(
    p: &mut CppParser,
    declarator_from: usize,
) -> ParseResult {
    let checkpoint = p.checkpoint();

    // A **macro invocation used where a definition goes**: `TEST(FormatPerformance, 1k_row) { … }`.
    //
    // This is how gtest, Catch2 and every benchmark library write a test, and the shape has no grammar behind it:
    // a name is *called* with tokens that are neither types nor expressions (`1k_row` is a token the macro pastes
    // into an identifier), and a block follows. The declaration reading gets as far as the group and then takes
    // the block for a braced initializer, so the definition has no `;` to end with and the whole statement is
    // reported as an expression — 6 of the 200 files in the first real C++ project, two of them with ~100
    // cascading errors from this one line each.
    //
    // Read here, and only in the shape that has no other reading:
    //
    // * **no declarator name** — `void f(A, B) { }` is an ordinary definition whose declarator is named `f`, and
    //   it never reaches this branch;
    // * **not inside a function body** — there, `g(x) { }` is a statement followed by a block, and a real error is
    //   the better answer;
    // * the group is **balanced** and a `{` follows it, which is the whole test. Nothing else can stand between a
    //   name and a block at declaration level, so the reading costs no valid program.
    //
    // The group is kept as an `ArgumentList` because that is what it is — a macro's arguments, not a parameter
    // list, and not an initializer either. The flag that says "a function declarator" is set so that the block is
    // read as the **body** by `parse_declaration` rather than as one more initializer.
    if a_macro_definition_follows(p, declarator_from) {
        parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)?;
        p.set_last_declarator_is_function(true);
        return Ok(CompleteMarker::empty());
    }

    // A declarator that named **nothing**, in a declaration whose head is a **qualified** name, is the head of a
    // definition — `static void Widget::draw(T)`, `void A::f<int>(int)` — so its parentheses are a parameter list
    // and nothing else.
    //
    // The initializer reading is for `Widget w(T)`, where the declarator *holds* the name `w`; here it holds
    // nothing, and the preference below is what got it wrong: `(T)` is a list of bare names, which is exactly the
    // shape the initializer reading claims, so the definition came out with no parameter list at all — and then
    // the `{` of its body had no declaration to belong to. A qualified name in type position is a definition
    // head, and the suffix reader is the only one that can say so.
    //
    // The parameter reading can still fail — `ns::C::method(1, 2);` is a *call* on a qualified name, and `1` is
    // not a type — and the rewind below hands those tokens back to the readings that follow.
    if !a_name_was_parsed(p, declarator_from) && the_head_of_the_declaration_is_qualified(p) {
        let before_the_parameters = p.checkpoint();
        if parse_parameter_list(p).is_ok() {
            p.set_last_declarator_is_function(true);
            super::types::eat_function_qualifiers(p);
            return Ok(CompleteMarker::empty());
        }
        p.rollback(before_the_parameters);
    }

    // An untyped list of names has to be claimed *before* the parameter reading is tried, and only here. A
    // parameter must have a type, but a bare name is also a perfectly good type, so `Max(a, b);` parses as a
    // parameter list — two parameters of type `a` and type `b` — and a reading that succeeds is never revisited.
    // At file scope that is the wrong answer for a shape that only a declaration can have, so the initializer
    // reading goes first and the parameter reading is what gets rewound instead. Inside a body the call is the
    // ordinary reading and the order is left alone; see [`a_declaration_is_the_better_reading`].
    //
    // This preference is for a declaration with **no type**, which is what `Max(a, b);` is. A type keyword in
    // front of the declarator's name settles the question the preference was guessing at, and the parameter
    // reading is the right one — `void f(T);` is a function with one unnamed parameter, not a variable `f`
    // initialised with the value `T`. Reading it as a variable was a *silent* wrong tree: well formed, lossless,
    // and no diagnostic. See [`a_type_keyword_precedes_the_declarator_name`].
    //
    // The second form is the same argument one level down: once the leading name *is* a type this file declared,
    // the parentheses follow a declarator name, and `Inner(1)` in them is a value being constructed rather than
    // a parameter. That reading is what a parameter list gets wrong — it takes `Inner` for a parameter's type
    // and then reads `(1)` as that parameter's default argument, which is a declaration of a function nobody
    // wrote.
    let declarator_is_named = p.has_declaration_type_name();
    if !a_dynamic_exception_specification_follows(p)
        && (p.is_at_file_scope()
            && !a_type_keyword_precedes_the_declarator_name(p)
            && the_arguments_look_like_declarators(p, false)
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
    if !a_dynamic_exception_specification_follows(p)
        && a_declaration_is_the_better_reading(p, declarator_from)
        && an_argument_list_follows(p)
    {
        return parse_the_initializer(p);
    }

    Ok(CompleteMarker::empty())
}

/// Is the balanced group at the cursor followed by a **dynamic exception specification** — `throw (`?
///
/// The one suffix that does not merely continue a reading but *settles* it: a variable declaration has no
/// `throw(…)`, so when the group at the cursor is followed by one, the group is a **parameter list** and the
/// direct-initialiser reading of the same tokens — the other reading of `T x(y)` — is not available.
///
/// Found because the two preferences below took the initialiser reading first and left the suffix with nothing to
/// belong to: `new_handler set_new_handler(new_handler) throw();` in `<new>` came out as an *expression statement*
/// of flat tokens with `expected `;` after expression`, and the whole declaration was lost. The group
/// `(new_handler)` is a bare name, which is a perfectly good one-parameter list *and* a perfectly good
/// parenthesised value, and the first of the two preferences is written for exactly that shape — so the
/// disagreement was between two readings that both succeed, and the suffix is what breaks the tie.
///
/// The suffix is not read here and nothing is consumed: the caller only asks whether the initialiser reading is
/// still the one to take, and the parameter reading that wins instead consumes `throw(…)` through
/// [`super::types::eat_function_qualifiers`].
fn a_dynamic_exception_specification_follows(p: &CppParser) -> bool {
    let Some(after) = index_after_the_group(p, p.current_token_index()) else {
        return false;
    };

    let throw_at = significant_index_at(p, after);
    p.token_kind_at(throw_at) == CppTokenKind::ThrowKeyword
        && p.token_kind_at(significant_index_at(p, throw_at + 1)) == CppTokenKind::LeftParen
}

/// Is the cursor on the argument list of a **macro invocation used where a definition goes**?
///
/// The shape is `TEST(FormatPerformance, 1k_row) { … }`, and it is the one shape in a declaration where the
/// declaration/expression question has no answer at all: the tokens inside the parentheses are the macro's, so
/// they are neither values nor declarators, and both readings refuse them. The test is therefore about the shape
/// and nothing else — see [`parse_function_suffix_or_initializer`] for the reading:
///
/// * **no declarator name**, so `void f(A, B) { }` — an ordinary definition — never matches;
/// * **not a qualified head**: `void Widget::draw(T) { }` has no declarator name either (the specifier sequence
///   took the whole qualified name), and its parentheses *are* a parameter list. A `::` in the head is what
///   tells the two apart, and the definitions of out-of-line members are far too common to lose;
/// * a **balanced** group with a `{` right after it;
/// * and, **inside a function body**, a name written the way a macro is written — see
///   [`looks_like_a_macro_name`] for why that half is a convention and which mistake it keeps.
///
/// Exposed because the declarator's suffix loop has to ask it *before* it opens at all: its other reasons to open
/// are all answers to the declaration/expression question, and this shape has none of them — which is why
/// `TEST(A, B) { }` (whose arguments look like declarators) worked while `TEST(A, 1) { }` (whose arguments look
/// like values) did not.
pub(super) fn a_macro_definition_follows(p: &CppParser, declarator_from: usize) -> bool {
    !a_name_was_parsed(p, declarator_from)
        && !the_head_of_the_declaration_is_qualified(p)
        && a_block_follows_the_group(p)
        // At *declaration* level the shape has no other reading at all, so any name will do. Inside a **body**
        // it competes with a real mistake — a call whose `;` is missing, followed by a block — so the name has to
        // be evidence: a macro this file `#define`d, or one the caller's table describes, and only then the
        // spelling convention as the last resort (a macro from a header nobody indexed).
        //
        // The statement rule claims most of these shapes before this one is reached, so this consult is normally
        // redundant — and it is here anyway, because a rule that *depends* on another rule running first is a rule
        // whose behaviour changes when the order changes. Evidence first, convention second, in both rules.
        && (!p.is_inside_a_body()
            || p.declaration_type_name().is_some_and(|name| {
                p.macro_evidence(name)
                    .is_some_and(MacroEvidence::may_be_a_statement_without_a_semicolon)
                    || looks_like_a_macro_name(name)
            }))
}

/// Is this name written the way a **macro** is written: `TEST`, `IF_EXIST`, `CHECK_EQ`?
///
/// This is a *convention* rather than a grammar rule, and it is used in exactly one place: the "macro invocation
/// used where a definition goes" shape of [`a_macro_definition_follows`], and only where a name could otherwise
/// be a real mistake. Two cases, and they need different answers:
///
/// ```text
/// IF_EXIST(indent_style) { … }   a macro — the block is its body, and there is no other valid reading
/// g(x) { }                       a call with its `;` missing, followed by a block — a syntax error
/// ```
///
/// Both are "a name, a parenthesised group, a block", and no grammar rule separates them: the second is *not*
/// valid C++ at all, so reading it as a macro would silently accept a mistake a reader has to fix. What separates
/// them is how the name is spelled, and it is the same signal a reader uses. `EmmyLuaCodeStyle`'s
/// `IF_EXIST(...) { … }` is the case that made the inside-of-a-body half necessary — 68 diagnostics from the
/// macro invocations in one file.
fn looks_like_a_macro_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    name.chars().all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    })
}

/// Is the `(` at the cursor a **balanced** group with a `{` immediately after it?
///
/// The shape test for a macro invocation used as a definition — see
/// [`parse_function_suffix_or_initializer`]. Balanced rather than "parses as something", because the tokens
/// inside are the macro's, and they need not parse as anything at all.
fn a_block_follows_the_group(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    let mut depth = 0isize;
    let mut offset = 0usize;
    while let Some(kind) = p.peek_token_kind_at(offset..offset + 1).first().copied() {
        match kind {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return p
                        .peek_token_kind_at(offset + 1..offset + 2)
                        .first()
                        .copied()
                        == Some(CppTokenKind::LeftBrace);
                }
            }
            // A `;` before the group closes means this is not the shape at all: it is a declaration that ends.
            CppTokenKind::Semicolon if depth == 0 => return false,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
        offset += 1;
    }

    false
}

/// Consume a balanced parenthesised group as **raw tokens**, under a node of `kind`.
///
/// For the one shape that has no grammar behind it: the arguments of a macro invocation, whose tokens are the
/// macro's own. They are kept in the tree as they were written — lossless, and available to a consumer that wants
/// to show them — while nothing pretends to know what they mean.
pub(super) fn parse_balanced_token_group(p: &mut CppParser, kind: CppSyntaxKind) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(kind);

    let mut depth = 0isize;
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                depth += 1;
                p.bump();
            }
            CppTokenKind::RightParen => {
                depth -= 1;
                p.bump();
                if depth == 0 {
                    return Ok(m.complete(p));
                }
            }
            CppTokenKind::Eof | CppTokenKind::None => {
                p.close_marks_above(base);
                return Err(CppParseError::syntax_error_from(
                    "unterminated argument list",
                    p.current_token_range(),
                ));
            }
            _ => p.bump(),
        }
    }
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

    // What the **caller's table** says about the leading name, when the caller supplied one.
    //
    // This is the consumer the external-table design exists for. `Widget w(1, 2);` and `g(1, 2);` are the same
    // tokens, and the file's own table answers only for names *this* file declares — a class from a header, a
    // helper defined in another translation unit, are exactly the names that made `Widget w(1, 2)` a guess. The
    // index behind this table knows them.
    //
    // A `Function` or `Variable` answer is the part the local table can *never* express: "this name is not a
    // type". That is a reading being refused rather than one being chosen, and it is why the trait answers in
    // kinds rather than in a `bool` — see `crate::symbols`.
    //
    // A `None` — the ordinary case — falls through to the shape preferences below, which is what keeps this a
    // preference and not a dependency.
    if let Some(name) = p.declaration_type_name()
        && let Some(kind) = p.symbol_kind(name)
    {
        return match kind {
            SymbolKind::Type | SymbolKind::Template => true,
            // Not a type, so not a declaration's type: the reading is refused and the expression statement —
            // the call — is what the tokens really are.
            SymbolKind::Function | SymbolKind::Variable | SymbolKind::Namespace => false,
            // A macro in type position is not itself a type — what stands there is what its body produced. A table
            // that knows the body says `MacroBody::Type` for a type macro, and the shape rules handle the rest;
            // answering "not a type" here keeps a macro name from being taken for one.
            SymbolKind::Macro { .. } => false,
        };
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

/// Is the cursor on an initializer for a declarator that named nothing, in a declaration that has no type?
///
/// The shape that must not be a declaration, and it is worth naming so the guard at the call site reads as the
/// rule rather than as a conjunction of parser states:
///
/// ```text
/// int x = 1;          a name was parsed    -> a declaration, and the `=` initialises it
/// x = 1;              no name, no type     -> an *assignment*, and the expression reading owns it
/// int ns::count = 0;  no name, but a type  -> a declaration whose declarator was folded into the type
/// ```
///
/// The last line is why the condition is not simply "no name was parsed". A **qualified** declarator is read by
/// the specifier sequence rather than by the declarator rule — `ns::count` joins the type and nothing is left to
/// name — so the name test alone would refuse a declaration that is both valid and long-standing. What separates
/// the two is whether what is in front can be a type at all: `ns::count` is written as a type, while a bare
/// undocumented `x` is not.
///
/// That keeps the reading honest in both directions. `x = 1;` is an assignment, which is the shape this exists
/// for. `ns::count = 0;` keeps its declaration reading — the one C++ gives it when `count` is a static member,
/// and the reason `int ns::Widget::count = 0;` has always parsed.
///
/// A structured binding and a parenthesized declarator never reach this: both return before
/// [`finish_init_declarator`] is asked. What is left is the abstract declarator, which is legitimately nameless
/// in a type-id — and a declaration is not a type-id, so a nameless one with no type has nothing to initialise.
fn an_initializer_needs_a_name(p: &CppParser, declarator_from: usize) -> bool {
    matches!(p.current_token(), CppTokenKind::Assign)
        && !a_name_was_parsed(p, declarator_from)
        && !the_declaration_has_a_type(p, declarator_from)
}

/// Does the declaration being parsed already have a type — by any of the names a type can be written with?
///
/// The question the guard above turns on, and the reason it is not simply "was a name parsed". A name is what
/// [`CppParser::declaration_type_name`] records for `T x`, and the declarator rule takes it — so the *name* half
/// is covered by [`a_name_was_parsed`]. What is left is the shape where the type is a **keyword** and the
/// declarator was taken with it, which is `decltype(x) y = 1` and `auto x = 1`.
///
/// [`declarator_starts_with_a_type_keyword`] answers most of that, except that it leaves `auto` and `decltype`
/// out on purpose — both can begin an **expression**, and where the two readings are ambiguous the expression is
/// preferred. That reasoning does not apply here: by the time this is asked the declaration reading has already
/// been chosen and has reached its initializer, so the only question left is whether what came before it was a
/// type. So the walk is repeated with the two keywords put back.
///
/// # What this still does not cover
///
/// `decltype(x) y = 1;` is read as an expression and reported, and that is a **known gap** rather than a
/// regression — it is registered in `crates/cpp_parser/tests/gaps.rs`. The reason is worth recording, because it
/// is the opposite of what it looks like: with the guard removed the construct "parses", but only by way of the
/// defect the guard exists for — a declaration whose declarator named nothing. The walk finds `y` rather than
/// `decltype` because by the time the guard runs, `decltype(x)` has been consumed by an *earlier* attempt and the
/// remaining text is `y = 1`, whose first token is a plain name. Making the guard accept that would put A0-1
/// back. Fixing it properly means making `decltype(auto)` parse as a type specifier in an expression-statement
/// position, which is a rule of its own.
fn the_declaration_has_a_type(p: &CppParser, declarator_from: usize) -> bool {
    let _ = declarator_from;
    if declarator_starts_with_a_type_keyword(p) || declarator_starts_with_a_known_type_name(p) {
        return true;
    }
    if p.has_qualified_declaration_type_name() {
        return true;
    }

    // The two type keywords the rule above excludes, asked at the **start of the declaration**. The walk is that
    // rule's own — back over the consumed tokens, stopping at a `;`, a `{` or a `}`, which no declaration
    // contains — and trivia is skipped so the answer is about the first *significant* token rather than the
    // whitespace before the `=`.
    let mut index = p.current_token_index();

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        if matches!(
            kind,
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace
        ) {
            return false;
        }
        if is_declaration_trivia(kind) {
            continue;
        }

        return super::types::is_type_specifier_keyword(kind);
    }

    false
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
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
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
                return true;
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
                // The follower is read after the **whole qualified chain**, not just after its first segment:
                // a `::` says the name continues, so it is not an answer to "what follows this name".
                //
                // ```text
                // std::move(b)     a call — the `(` after `move` settles it
                // std::move(b.c()) likewise, and it is why the chain has to be walked at all
                // B::C             a type — the `,` or `)` after `C` settles it
                // std::string name a type and a name — `name` settles it
                // ```
                //
                // Stopping at the first `::` made a qualified *call* look like a type: the follower was `::`,
                // which keeps the element a declaration, and the element-start marker was spent — so the call's
                // own `(` and everything inside it went unread, and an argument like `std::move(luaLexer.
                // GetTokens())` produced no evidence of a value at all. The declaration reading was then refused
                // and the statement reported `expected ;` against its own `(`. Seven files of the first real C++
                // project put in front of this parser failed on that one shape.
                let mut after = next_significant_index(p, index);
                while p.token_kind_at(after) == CppTokenKind::Scope {
                    let segment = next_significant_index(p, after);
                    if p.token_kind_at(segment) != CppTokenKind::Identifier {
                        break;
                    }
                    after = next_significant_index(p, segment);
                }
                let follower = p.token_kind_at(after);
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
///
/// Exposed to `super::types`, which asks the same question about the name a specifier sequence is looking at.
pub(super) fn next_significant_index(p: &CppParser, index: usize) -> usize {
    significant_index_at(p, index + 1)
}

/// The first significant token **at or after** `index`.
///
/// The difference from [`next_significant_index`] is one token, and it is the difference between reading a
/// declaration and eating it: a scanner that has just finished a group holds the index **one past** the closing
/// paren, and that index may be sitting on the newline after it. Asking for the token *after* that one skips
/// whatever the group was followed by — which is how `size_t _Hash_bytes(const void*);` came out as two macro
/// invocations and the `;` landed on the next declaration.
fn significant_index_at(p: &CppParser, index: usize) -> usize {
    let mut index = index;
    while index < p.token_count() && is_declaration_trivia(p.token_kind_at(index)) {
        index += 1;
    }
    index
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

/// Did the declaration being parsed write a **qualified** name in type position?
///
/// ```text
/// void A::f<int>(int);          the head is `void A::f<int>`
/// static void Widget::draw(T);  the head is `static void Widget::draw`
/// int x = A::b;                 …and this is not a head with a qualified *type*, which the `;` and `=`
///                               before it are what rule out
/// ```
///
/// Asked of the **tokens** rather than of the name the specifier sequence recorded, because the record is empty
/// for the spelling that needs this most: `A::f<int>` ends in a template argument list, so the walk that recovers
/// a name stops on the `>` and reports nothing — and a declaration whose head names a qualified type is the head
/// of a *definition*, whose parentheses are a parameter list and whose declarator has no name of its own.
///
/// The walk is the one the other readers of a declaration's head use: backwards from the cursor to the start of
/// the declaration, stopping at a `;`, `{` or `}`, so it cannot run out of this declaration and answer for the
/// one before it. Trivia needs no skipping — none of it is a `::`.
pub fn the_head_of_the_declaration_is_qualified(p: &CppParser) -> bool {
    let mut index = p.current_token_index();

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        if matches!(
            kind,
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace
        ) {
            return false;
        }
        if kind == CppTokenKind::Scope {
            return true;
        }
    }

    false
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
/// It also steps over **angle lists**, and stops at a template head's `template`. Both were bugs rather than
/// refinements, and they were the same bug: the walk collected the token that followed the earliest boundary,
/// so for a declaration under a head it collected the `<` of `template <…>` and answered "not a type keyword"
/// for every one of them.
///
/// The symptom was narrow enough to hide and wide enough to matter: the suffix loop of an **unnamed** declarator
/// opens only when this answers yes (a named one opens on its own name), so
/// `template <class T> void f(int[4]);` failed with ``expected a parameter list or an initializer`` — the
/// parameter's `[4]` had nothing to attach to — while `void f(int[4]);` was read, and while the same parameter
/// *with* a name was read too. Measured: `bits/stl_pair.h`, `concepts` and `bits/functional_hash.h` each stopped
/// at a line of this shape.
///
/// The angle stepping is the other half and is what makes the stop at `template` mean anything: a list's own
/// tokens are not the declaration's beginning — `std::vector<int> x` begins at `std::vector` — so both halves of
/// an angle list are stepped over rather than assigned.
///
/// Not asked of the event stream, which was the first attempt and does not work: the events reach back to the
/// beginning of the file, so the `BuiltinType` of an enclosing declaration is indistinguishable from this
/// one's by node kind alone.
fn declarator_starts_with_a_type_keyword(p: &CppParser) -> bool {
    let mut index = p.current_token_index();
    let mut first = None;
    // How many angle lists the walk is inside, counted **backwards**: a `>` opens one, its `<` closes it.
    let mut angles = 0usize;

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        if matches!(
            kind,
            CppTokenKind::Semicolon
                | CppTokenKind::LeftBrace
                | CppTokenKind::RightBrace
                | CppTokenKind::TemplateKeyword
        ) {
            break;
        }
        if is_declaration_trivia(kind) {
            continue;
        }

        match kind {
            CppTokenKind::Greater => angles += 1,
            CppTokenKind::RightShift => angles += 2,
            // The `<` that closes the list being walked through is stepped over like the tokens inside it.
            CppTokenKind::Less if angles > 0 => {
                angles -= 1;
                continue;
            }
            _ => {}
        }
        if angles > 0 {
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

/// Is a **type keyword** written immediately before the declarator's name — the `void` in `void f(T)`?
///
/// The question the direct-initialisation heuristic has to ask at the `(` of a declarator, and one
/// [`declarator_starts_with_a_type_keyword`] cannot answer: that one reads the *first* token of the whole
/// declaration, which a template head pushes away — `template <typename T> void f(T)` begins with `template`.
/// Here the walk starts at the declarator's own name, so the head is behind it and the type is in front.
///
/// A qualified name is stepped over as one thing: in `void A::f(T)` the name before the `(` is `A::f`, and the
/// type is still the `void` in front of it.
///
/// The answer matters because a declaration that *has* a type must take the parameter reading of the
/// parentheses, however its contents are spelled:
///
/// ```text
/// void f(T);        a function with one unnamed parameter of type `T`
/// template <typename T> void f(T) { }
/// ```
///
/// Both used to be read as *variables* — `f` initialised with the value `T` — because a list of bare names is
/// the one shape a direct-initialiser and a parameter list share, and at file scope the initialiser reading was
/// preferred for it. That preference exists for `Max(a, b);`, a declaration with **no type at all**; a `void` in
/// front of the name settles the question the preference was guessing at.
fn a_type_keyword_precedes_the_declarator_name(p: &CppParser) -> bool {
    let mut index = p.current_token_index();
    let mut seen_an_identifier = false;

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        if is_declaration_trivia(kind) {
            continue;
        }
        if matches!(
            kind,
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace
        ) {
            return false;
        }

        match kind {
            CppTokenKind::Identifier => seen_an_identifier = true,
            // A `::` in front of the name just read: this is a qualified name, so the walk continues to the
            // segment in front of it.
            CppTokenKind::Scope if seen_an_identifier => seen_an_identifier = false,
            // The token in front of the name — `auto` and `decltype` are left out for the reason
            // [`declarator_starts_with_a_type_keyword`] gives: both can begin an expression as readily.
            _ if seen_an_identifier => {
                return super::types::is_type_specifier_keyword(kind)
                    && !matches!(
                        kind,
                        CppTokenKind::AutoKeyword | CppTokenKind::DecltypeKeyword
                    );
            }
            _ => return false,
        }
    }

    false
}

/// Does a declarator begin where the cursor stands?
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

    // Where the declarators begin. "Did this declaration name anything?" has to be asked of the events *they*
    // produced — the type's own name is already in the stream behind this bound, and it would answer for them.
    let declarator_from = p.current_event_count();

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

    // A `for` header has no `;` of its own, so a declaration that consumed only a **type** and named nothing has
    // nothing left to fail on:
    //
    // ```text
    // for (;; i++) { }     `i` was read as the type, the declarator came out empty, and the header then reported
    //                      `expected )` against the `++` — the increment was never read as an expression at all
    // ```
    //
    // The ordinary statement path cannot reach that state: its declaration reading has a `;` to insist on, so
    // `i++;` fails there at the `++` and the expression reading takes over. The header is the one place the
    // question has to be asked directly, and it is asked the same way `an_initializer_needs_a_name` asks it —
    // of the events the declarators produced.
    //
    // Nothing legitimate is refused: a `for` init that declares something always names it (`int i = 0`,
    // `Widget w(1)`, `auto [a, b] = pair`), and a type on its own declares nothing to loop over. What the
    // refusal buys is the *fallback*: the caller rewinds and reads the header as an expression, which is what
    // `i++`, `i++, k++` and `v` in `for (v : m)` all are.
    if !a_name_was_parsed(p, declarator_from) {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected a declaration that names something",
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

/// Parse `{ ... }` as an initializer, keeping it as one node.
pub fn parse_braced_initializer(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::InitListExpr);

    expect_token(p, CppTokenKind::LeftBrace)?;

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        // A **preprocessor directive** between the elements. The statement rule reads a directive when it meets
        // one at the start of a statement, but an initializer is not a statement list — and a table whose rows are
        // conditional is exactly what conditional compilation is for:
        //
        // ```c
        // char const info_version[] = {
        //   'I', 'N', 'F', 'O', ':',
        // #ifdef COMPILER_VERSION
        //   COMPILER_VERSION,
        // #endif
        //   '\0' };
        // ```
        //
        // It is read as the node it is — the tree stays lossless and a consumer can still see the directive —
        // rather than reported as a missing element. Only *here* is the directive claimed: in an expression proper
        // a `#` is still an error, so nothing that used to be reported stops being reported.
        if p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            continue;
        }

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
        } else if p.current_token() != CppTokenKind::Hash {
            // A directive needs no comma in front of it: `'a',\n#ifdef X\n'b',` has one, but
            // `'a'\n#ifdef X\n#endif\n, 'b'` is a row that is *entirely* conditional and has none.
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

/// **The other branches' member initializer lists**, when a conditional repeats the list.
///
/// ```cpp
/// basic_string()                                   // bits/cow_string.h:515
/// #if _GLIBCXX_FULLY_DYNAMIC_STRING == 0
///   : _M_dataplus(_S_construct(size_type(), _CharT(), _Alloc()), _Alloc())
/// #else
///   : _M_dataplus(_S_construct(size_type(), _CharT(), _Alloc()), _Alloc())
/// #endif
///   { }
/// ```
///
/// One declarator, one initializer list **per branch**. Reading a single list and returning left the `#else` and
/// the second `:` to the matcher, which has no reading for either: the constructor failed, and the recovery then
/// took its `{ }` for the class's closing brace — so `class basic_string` ended 3400 lines early and every member
/// after it was read at file scope (`docs/grammar-gaps.md`, tenth round). A directive and a `:` are the only two
/// tokens this loop accepts, so it stops at the first body brace, `;` or anything else.
fn parse_further_member_initializer_lists(p: &mut CppParser) -> ParseResult {
    loop {
        let mut saw_a_directive = false;
        while p.current_token() == CppTokenKind::Hash {
            super::stats::parse_preprocessor_directive(p)?;
            saw_a_directive = true;
        }

        // The `:` of the next branch, and only after a directive: without one, the first list would be read
        // again — `Foo() : a(1)` has one `:`, and a loop that took every `:` would take a *bit-field*'s.
        if !saw_a_directive || p.current_token() != CppTokenKind::Colon {
            return Ok(CompleteMarker::empty());
        }

        parse_member_initializer_list(p)?;
    }
}
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
        // One **element** of the list, read below the comma operator: the commas between elements belong to the
        // list, not to an expression. See [`super::exprs::parse_assignment_expr`].
        if let Err(err) = super::exprs::parse_assignment_expr(p) {
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

    // An **explicit object parameter** (C++23): `void f(this S& self)`, `void f(this auto&&) &&`.
    //
    // `this` in parameter position is the start of a *type* — the deduced type of the object — and that is the
    // whole reason this needs its own rule: everywhere else `this` is an expression, so the specifier sequence
    // cannot read it, and the failure was **silent**. The parameter list gave up at the `this`, the declaration
    // reading failed, and the member came out as nothing at all while the tokens that followed were read as a
    // second, phantom member. No diagnostic was produced, so `gaps.rs` could not see it either — it was found by
    // probing for constructs rather than by a test.
    //
    // The shape after `this` is a **type-id** — `S&`, `S&&`, `auto&&` — and reading it as one is what covers
    // every spelling without a case each. It has to be a type, too: the operator `this` is an expression and
    // cannot begin a parameter, so a `this` with no type after it is refused here and left to whatever rule owns
    // it (`f(this);` is a call passing the object).
    //
    // The `this` is kept as a `ThisExpr` node so the text stays where it was written; a consumer looking for the
    // object parameter finds it by that node.
    if p.current_token() == CppTokenKind::ThisKeyword {
        let object = p.mark(CppSyntaxKind::ThisExpr);
        p.bump(); // `this`
        object.complete(p);

        // The type of the object, suffixes included: `S&`, `S&&`, `auto&&`. `parse_type_id` reads the whole
        // type-id, which is exactly what is written here — and its refusal is what makes the guard above
        // unnecessary: `this` followed by something that is not a type does not parse as one, so the caller
        // rewinds and the expression reading gets `f(this)`.
        if let Err(err) = super::types::parse_type_id(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        // The name, if there is one — `this S&` needs no more than the type.
        if p.current_token() == CppTokenKind::Identifier
            && let Err(err) = parse_name(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }

        // A default argument, read exactly as any other parameter's.
        if p.current_token() == CppTokenKind::Assign {
            p.bump();
            if let Err(err) = super::exprs::parse_assignment_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        return Ok(m.complete(p));
    }

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

    // Attributes on the parameter: `void f(int x [[maybe_unused]])`, the same position as on a variable. Read
    // after the declarator and before the default argument, which is the order C++ writes them in — and only
    // here, so the `[[` of an attribute written *before* the type goes on being read by the specifier sequence,
    // which is where it belongs.
    if let Err(err) = super::types::parse_attribute_specifiers(p) {
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

/// Does a **member that is only a macro invocation** start here — `Q_OBJECT`, `Q_PROPERTY(int x READ x)`?
///
/// Three conditions, and each is evidence or shape rather than a guess:
///
/// * the name is a macro — this file's own `#define`, or the caller's table (see `CppParser::macro_evidence`);
/// * the body, when the table describes one, is **not** a specifier or a type: `#define MY_INT int` used as
///   `MY_INT x;` is a declaration, and a table that says `Specifier` settles that on its own;
/// * a **`(`** invocation must not be followed by a `;`, or it is a plain call statement, which the statement rule
///   owns rather than the member loop.
///
/// The bare spelling is where the two sources of evidence part company, and the distinction is worth recording: a
/// *described* body that is a statement cannot be part of a declaration's specifiers, so the name is a macro
/// whatever follows it — which is the only way `Q_OBJECT` can be told from the next member, since that member
/// begins with a name and so looks exactly like a declarator. A name this file merely `#define`d has **no body to
/// consult** — the replacement list is not interpreted — so there the shape has to answer, and it answers
/// conservatively: no declarator may follow.
fn at_a_macro_member(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier {
        return false;
    }

    let Some(evidence) = p.macro_evidence(p.current_token_text()) else {
        return false;
    };
    if !evidence.may_stand_alone_as_a_member() {
        return false;
    }

    if p.peek_next_token() == CppTokenKind::LeftParen {
        // `Q_PROPERTY(…)`: the group is the macro's, and a `;` after it would make this an ordinary call
        // statement — which the statement rule owns, not the member loop.
        return !a_semicolon_follows_the_group(p);
    }

    match evidence {
        // A described body that is a statement settles it: a statement cannot be a declaration's specifiers, so
        // the name is a macro whatever follows — which is the only way `Q_OBJECT` can be told from the member
        // written after it, that member beginning with a name and so looking exactly like a declarator.
        MacroEvidence::Described { .. } => true,
        // A name this file `#define`d has no body to consult, so the shape answers, and it answers
        // conservatively: `#define MY_INT int` used as `MY_INT x;` is a declaration and must stay one.
        MacroEvidence::DefinedHere => !super::types::a_declarator_still_follows_the_name(p),
    }
}

/// Does a **call-shaped member nothing else could read** start here — a name, a parenthesised group, no `;`?
///
/// Kept as a named question even though **nothing calls it**, because the shape it answers for is real and the
/// answer is "not this way": `bits/stl_vector.h:464` writes `__glibcxx_class_requires(_Tp, _SGIAssignableConcept)`
/// on a line of its own, and that macro is defined in an *included* file, so [`at_a_macro_member`] cannot see it.
/// Reading the shape as a macro was tried in `parse_class_body_members` and reverted — the note there has the
/// measurement (111 members of `std::basic_string` for no file) and the reason. Left here so the next reader finds
/// the question already asked rather than re-deriving it.
#[allow(dead_code)]
fn at_a_call_shaped_macro_member(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::LeftParen
        && !a_semicolon_follows_the_group(p)
}

/// The kind of the first token **after** the balanced group at the cursor.
///
/// Read a **macro standing among a declarator's suffixes**, and say whether there was one.
///
/// `_GLIBCXX_NOEXCEPT`, `_GLIBCXX_NOTHROW`, `_GLIBCXX_USE_NOEXCEPT`, `_GLIBCXX20_DEPRECATED_SUGGEST("…")`,
/// `_GLIBCXX20_INIT({})` — one of these stands after nearly every declaration libstdc++ writes, and the whole
/// declaration was lost without it: `inline void __terminate() _GLIBCXX_USE_NOEXCEPT` reported `expected ';'`
/// against the macro, and then recovered by eating the body.
///
/// # Why the shape is decisive here
///
/// After a declarator, an identifier has exactly **two** readings in C++, and both are known: a contextual
/// keyword, or a macro. Everything else that may legally stand in this position is a keyword or punctuation —
/// `{`, `;`, `=`, `,`, `:`, `[[`, `->`, `noexcept`, `const`. So a name here costs no valid program, and the
/// alternative is an error; that is maintenance convention 16's fallback side of the rule, the same footing as
/// `eat_namespace_head_macros`.
///
/// The three names that would be a *better* reading as something else are refused by spelling, which is the same
/// test the function-suffix loop already makes for `override` and `final`: `override` and `final` are read by
/// that loop, and `requires` begins a clause the declarator loop reads for itself — taking it here would swallow
/// the constraint and leave the clause's tokens on the declaration that follows.
///
/// # Why the group is optional
///
/// Both spellings occur and they mean different things to the macro, not to this: `_GLIBCXX_NOEXCEPT` is a bare
/// name, `_GLIBCXX_NOEXCEPT_IF(noexcept(…))` is an invocation. The group is read as raw tokens by
/// [`parse_balanced_token_group`] — the macro's arguments, nothing interpreted — and the whole thing becomes one
/// `MacroCall`, the same node every other macro reading produces.
pub(super) fn eat_a_macro_suffix(p: &mut CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier {
        return false;
    }

    if matches!(p.current_token_text(), "override" | "final" | "requires") {
        return false;
    }

    let checkpoint = p.checkpoint();
    let m = p.mark(CppSyntaxKind::MacroCall);

    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    if p.current_token() == CppTokenKind::LeftParen
        && parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_err()
    {
        // An unterminated group is not this shape at all: give the name back and let the caller report whatever
        // it reported before.
        p.rollback(checkpoint);
        return false;
    }

    m.complete(p);
    true
}

/// Read a macro invocation that stands where a class member goes, into a `MacroCall`.
///
/// The same node the statement rule produces, for the same reason: a macro's meaning is not knowable here, and
/// dressing it up as a declaration would hide that.
fn parse_macro_member(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::MacroCall);

    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    if p.current_token() == CppTokenKind::LeftParen {
        parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)?;
    }

    // A `;` is not part of the shape this rule is for (that shape is a plain call statement), but a file may
    // write one, and swallowing it keeps the loop from reporting it as a stray member.
    if p.current_token() == CppTokenKind::Semicolon {
        p.bump();
    }

    Ok(m.complete(p))
}

fn parse_class_body_members(p: &mut CppParser) -> ParseResult {
    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        let member_base = p.open_marks();

        // A **directive between two members** — the class-scope side of the seam `parse_try_statement` and
        // `parse_if_statement` document, and the one that decides whether a standard-library class has members at
        // all:
        //
        // ```cpp
        // class basic_string {
        //   …
        // protected:
        // #if __cplusplus < 201103L          // ← here
        //   typedef iterator __const_iterator;
        // #else
        //   typedef const_iterator __const_iterator;
        // #endif
        // ```
        //
        // Without this, the `#if` line was read as a **member declaration** whose declarator name is
        // `__cplusplus`, and every declaration after it became a child of that bogus member: the tree stayed
        // lossless and free of diagnostics, the class's *own* members were no longer members, and
        // `bits/basic_string.h` indexed seven typedefs and not one method. See `docs/roadmap.md` §2.1 — the
        // failure mode here is a wrong *shape*, which is why no error-based check could see it.
        //
        // Read as the node it is, then go round the loop again: the next token is a member, an access specifier, or
        // the closing brace, and each of those is a case the loop already handles.
        if p.current_token() == CppTokenKind::Hash {
            super::stats::parse_preprocessor_directive(p)?;
            continue;
        }

        if matches!(
            p.current_token(),
            CppTokenKind::PublicKeyword
                | CppTokenKind::PrivateKeyword
                | CppTokenKind::ProtectedKeyword
        ) {
            parse_access_specifier(p)?;
            continue;
        }

        // A **member that is nothing but a macro invocation**: `Q_OBJECT`, `Q_PROPERTY(int x READ x)`,
        // `Q_ENUM(E)`. An attribute-like macro is written where a member goes and carries no `;` — its expansion
        // supplies whatever declarations it wants — so the member loop has to recognise it before the declaration
        // rule takes the name for a type and then fails at the next member.
        //
        // The evidence is the table's, or this file's own `#define` (see `CppParser::macro_evidence`), and the
        // shape test is what keeps a *declaration* out of it: `#define MY_INT int` used as `MY_INT x;` has a
        // declarator after the name, so it is a declaration and never reaches this rule.
        let before = p.current_token_index();
        if at_a_macro_member(p) {
            parse_macro_member(p)?;
            continue;
        }

        // # Why a **call-shaped member** is deliberately *not* read here
        //
        // The shape is real — `bits/stl_vector.h:464` writes `__glibcxx_class_requires(_Tp, _SGIAssignableConcept)`
        // on a line of its own, and its macro is defined in an *included* file (`bits/c++config.h`), so
        // [`at_a_macro_member`] cannot see it. Reading "a name, a group, and no `;`" as a macro **after the
        // declaration reading fails** was tried: the shape was asked before the attempt (a failed attempt does not
        // put the cursor back — a `parse_declaration` that gives up has already consumed the name and stopped on
        // the `(`), the checkpoint was rewound only on failure, and the reading is correct in isolation. It was
        // still **reverted**, because it is measurably worse than the recovery it replaces:
        //
        // ```text
        // declarations_in("std::basic_string")   398 → 287   (closure of <string>/<vector>/<map>/<algorithm>)
        // bits/stl_vector.h's first error        540 → 540   (it bought nothing on the file it was written for)
        // ```
        //
        // The reason is the one thing the error-node recovery does that a macro reading cannot: it **advances by
        // one token** and lets the loop try again, so a declaration this parser cannot read costs its own first
        // token and the members written after it are still members. A macro reading takes the name *and* the
        // group, and whatever that group was the beginning of is gone. The standard library charged 111 members
        // for it.
        //
        // So the queue entry stands (`roadmap.md` §2.3) and the evidence it needs is **macro evidence**, not
        // shape: either the macro environment (P3) or a source that reaches `bits/c++config.h`.

        // A member is a declaration; anything that is not gets wrapped in an error node so the
        // loop always advances.
        //
        // The nodes the failed attempt opened are closed **with** their end events
        // ([`MarkerEventContainer::end_marks_to`], not `close_marks_above`): a member declaration that fails
        // after consuming tokens — `_GLIBCXX20_CONSTEXPR void f(size_type) { }`, `int x = 1` with no `;` — would
        // otherwise leave an unpaired `NodeStart` that the tree builder balances at the end of the file, so the
        // abandonment swallowed every member written after it. See the note on `end_marks_to`.
        if parse_member(p).is_err() {
            p.end_marks_to(member_base);
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
/// Does the `decltype` at the cursor open a **type**, rather than the start of an expression?
///
/// `decltype(x)` is both, and the statement rule needs the answer before it chooses a reading:
///
/// ```text
/// decltype(x) y = 1;    a declaration — the `y` is a declarator
/// decltype(auto) x = f();   likewise
/// decltype(x) + 1;      an expression — the `+` is not one
/// decltype(x)::value    an expression — a `::` continues the *name*, not a declaration
/// ```
///
/// The test is the same speculative one the type table's callers use, and it is safe to make here because the
/// type reading is the one that gets rewound: the type-id is read, and the question is only what stands after
/// it. `parse_type_id` refuses `decltype(x) + 1` on its own — a `+` cannot continue a type — so the walk is
/// needed only for the shapes a type *can* be followed by and a declaration cannot, which is the `::` above.
fn a_decltype_here_is_a_type(p: &mut CppParser) -> bool {
    let checkpoint = p.checkpoint();
    let parsed = super::types::parse_type_id(p).is_ok();
    let followed_by_a_declarator = parsed && starts_a_declarator(p);
    p.rollback(checkpoint);
    followed_by_a_declarator
}

/// The kind of the first significant token **after** the one at the cursor.
///
/// The question a rule asks when the token at the cursor is a name and what decides the reading is what follows,
/// and it asks it about a **run**: libstdc++ writes two macro names where a declaration goes, and sometimes
/// three —
///
/// ```text
/// _GLIBCXX_BEGIN_NAMESPACE_VERSION
/// _GLIBCXX_BEGIN_NAMESPACE_CONTAINER
///   template <typename> struct _List_iterator;    <- the run ends here, at `template`
/// ```
///
/// — so stopping at the first token after the name would answer "an identifier follows" and refuse a shape whose
/// answer is three tokens further on. A group after a name is stepped over as part of it (`MACRO(a) MACRO(b)
/// template<…>`), because the group is that invocation's.
///
/// What it does **not** do is decide anything: it returns the first token that is neither a name nor a group, and
/// the caller asks its own question about that. `Widget w;` and `x = 1;` both stop on their second token, which
/// is why the run is not a licence to skip a declaration.
pub(super) fn kind_after_the_run_of_names(p: &CppParser) -> CppTokenKind {
    let mut index = next_significant_index(p, p.current_token_index());

    loop {
        // A group belongs to the name in front of it, and the first pass is the name at the **cursor**:
        // `_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(_V2)` is one invocation wherever it is asked about, so a scanner
        // that only stepped over groups *after* names it had already passed would stop on its own `(` and answer
        // `LeftParen` — which is not a declaration start, so the invocation was refused and the declaration
        // behind it came out as an error.
        if p.token_kind_at(index) == CppTokenKind::LeftParen {
            let Some(after) = index_after_the_group(p, index) else {
                return CppTokenKind::None;
            };
            index = significant_index_at(p, after);
            continue;
        }

        if p.token_kind_at(index) != CppTokenKind::Identifier {
            break;
        }
        index = next_significant_index(p, index);
    }

    p.token_kind_at(index)
}

/// The kind of the first significant token after the balanced group at the cursor.
///
/// The question a rule asks when the cursor is on a macro invocation's group and the reading depends on what
/// comes next. [`CppTokenKind::None`] when the group never closes: an unbalanced `(` has no "after", and a caller
/// that read one would be answering about the wrong token.
pub(super) fn kind_after_the_group(p: &CppParser) -> CppTokenKind {
    match index_after_the_group(p, p.current_token_index()) {
        Some(after) => p.token_kind_at(significant_index_at(p, after)),
        None => CppTokenKind::None,
    }
}

/// The index one past the balanced group that opens at `index`, or `None` if it never closes.
///
/// The one place parentheses are counted, so that the four questions asked about "what follows a group" cannot
/// come to disagree about where a group ends.
fn index_after_the_group(p: &CppParser, index: usize) -> Option<usize> {
    let mut depth = 0isize;
    let mut index = index;

    while index < p.token_count() {
        match p.token_kind_at(index) {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            CppTokenKind::Eof => return None,
            _ => {}
        }
        index += 1;
    }

    None
}

/// Is the balanced group at the cursor followed by a `;`?
pub(super) fn a_semicolon_follows_the_group(p: &CppParser) -> bool {
    kind_after_the_group(p) == CppTokenKind::Semicolon
}

/// Can a declaration begin with this token kind?///
/// Factored out of [`starts_declaration`] because there are two starting points and one question:
/// `starts_declaration` asks it at the **cursor**, where a statement could also begin, and
/// [`super::stats::at_a_macro_that_stands_for_a_declaration`] asks it at the token **after a name**, where the
/// other reading is an expression that continues. Two copies of this list would be free to disagree about what
/// begins a declaration, and the list is a closed grammatical set — every entry says "a declaration may start
/// with this", which is a different kind of claim from the "which token kinds may be inside a header name" list
/// whose cost `docs/grammar-gaps.md` entry 22 records.
///
/// The kinds whose answer needs the tokens *after* them (`decltype`, and `concept`, which is contextual) are not
/// here: they stay in [`starts_declaration`], where that lookahead is available.
pub(super) fn can_begin_a_declaration(kind: CppTokenKind) -> bool {
    matches!(
        kind,
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
            | CppTokenKind::AlignasKeyword
            // Specifiers. Several of these also begin expressions — `const` cannot, `static` cannot, `auto`
            // cannot — which is what the caller's lookahead is for.
            | CppTokenKind::ConstKeyword
            | CppTokenKind::VolatileKeyword
            | CppTokenKind::ConstexprKeyword
            | CppTokenKind::StaticKeyword
            | CppTokenKind::InlineKeyword
            | CppTokenKind::VirtualKeyword
            | CppTokenKind::ExplicitKeyword
            | CppTokenKind::FriendKeyword
            | CppTokenKind::MutableKeyword
            | CppTokenKind::ThreadLocalKeyword
    )
}

pub fn starts_declaration(p: &mut CppParser) -> bool {
    match p.current_token() {
        // `concept` is **not** an anchor, and the omission is a decision: the word is contextual, and a statement
        // that begins with it is an ordinary use of the name — `concept = 2;`, `concept();`. A concept definition
        // always begins with `template`, which is already an anchor, so nothing is lost by leaving the bare word
        // to the expression rule. Anchoring it here is what made `void f() { concept = 2; }` demand a concept name
        // at the `=`.
        //
        // `alignas` is an anchor and is not an optimisation: without it `alignas(16) E e;` with an unqualified
        // name for the type reaches the expression rule, fails at `alignas`, and reports `expected primary
        // expression` against a token the specifier loop had just learned to read.
        //
        // `decltype(x) y = 1;` and `decltype(auto) x = f();` — and only those. A `decltype` **can** begin an
        // expression (`decltype(x) + 1;`), so it is not an anchor on its own; [`a_decltype_here_is_a_type`] asks
        // whether what follows the type-id is a declarator, which is the same question the two readings differ on.
        //
        // Without the anchor the declaration reading is never taken at all, because `decltype` is in
        // `is_expression_keyword` and no expression rule consumes it — so the statement rule read it as a name,
        // found `y` next, and reported `expected primary expression` against the `decltype` itself.
        CppTokenKind::DecltypeKeyword if a_decltype_here_is_a_type(p) => true,

        kind => can_begin_a_declaration(kind),
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
    parse_declaration_starting_at_a_name(p, super::types::parse_declarator)
}

/// Parse a **conversion** operator declaration: `operator int();`, `operator bool() const;`,
/// `operator std::string() const &;`, `operator const char*() = delete;`.
///
/// Same shape as a destructor declaration, for the same reason and with the same fix: the name is written
/// first and no type precedes it, so the specifier sequence — which runs first in the general rule — has
/// nothing to read and refuses the whole declaration.
///
/// What it did instead was worse than a refusal. `operator` was left as an error node, `int` was read as a
/// declaration of a variable named `int` — which also taught the file's type table that `int` is a type
/// *this file declared* — and the `()` became a further error. No error was reported, so the only symptom
/// was a member list missing its conversion operator.
///
/// Reached from [`parse_declaration`] when the declaration's first token is `operator`, which no other
/// declaration can begin with: an *overloaded* operator's name is preceded by its return type
/// (`Ops operator+(const Ops&)`), so by the time this dispatch is reached the payload cannot be one.
fn parse_conversion_operator_declaration(p: &mut CppParser) -> ParseResult {
    parse_declaration_starting_at_a_name(p, |p| {
        // The name, then the declarator's suffixes: the parameter list of the operator, its cv-qualifiers and
        // its ref-qualifier. `parse_declarator` cannot be used here because it would try to read a *declarator*
        // at a token that is the name — there is no type in front of it for a declarator to follow.
        let name = p.mark(CppSyntaxKind::Declarator);
        if let Err(err) = parse_name(p) {
            name.undo(p);
            return Err(err);
        }
        name.complete(p);
        super::types::parse_declarator_function_suffixes(p, p.current_event_count())
    })
}

/// The tail every declaration shares: one init-declarator, then a body, then a `;`.
///
/// Factored out for exactly two callers — a destructor and a conversion operator — which are the two
/// declarations whose *name* begins the declaration and which therefore cannot go through the specifier
/// sequence. Having two copies of "the body or the `;`" is how the first copy of it came to report
/// `expected ';'` against the `}` of `~S() {}`.
fn parse_declaration_starting_at_a_name(
    p: &mut CppParser,
    parse_the_name: impl FnOnce(&mut CppParser) -> ParseResult,
) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declaration);

    let declarator_from = p.current_event_count();

    if let Err(err) = parse_the_name(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    if let Err(err) = finish_init_declarator(p, m, declarator_from) {
        p.close_marks_above(base);
        return Err(err);
    }

    // A body, or its `;`. A declarator whose parameter list has been read is a *function*, so the brace after
    // it is a definition rather than a braced initializer.
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

    // An attribute on the alias's name: `using T [[deprecated]] = int;`. Between the name and the `=`, which is
    // the same position an attribute takes on any other declarator — see `finish_init_declarator` — and the one
    // this rule does not share with it, because an alias has no declarator node of its own.
    if let Err(err) = super::types::parse_attribute_specifiers(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The type's **suffixes**: `using Arr = int[4];`, `using Fn = int(char);`, `using P = int(*)[4];`.
    //
    // A type-id reads the abstract declarator — the pointers, references and cv-qualifiers — and stops
    // before the array and function parts, because everywhere else a type-id appears those belong to
    // whatever encloses it. In an alias there is nothing enclosing it: the `=` was the last thing before
    // the type and the `;` is the last thing after it, so anything left is part of the type.
    //
    // That asymmetry is why `typedef int Arr[4];` worked while `using Arr = int[4];` did not — the typedef
    // rule reaches the suffixes through `parse_declarator`, and this rule had no equivalent step.
    //
    // The definition is read **once per branch** (`#if A = X; #else = Y; #endif`), which is how
    // `make_integer_sequence` is written; see [`parse_a_definition_per_branch`].
    let mut declared = false;
    let ended = parse_a_definition_per_branch(p, |p| {
        if !declared {
            if let Some(name) = &alias_name {
                p.declare_type_name(name);
            }
            declared = true;
        }

        parse_type_id(p)?;

        if (p.current_token() == CppTokenKind::LeftBracket
            || p.current_token() == CppTokenKind::LeftParen)
            && let Err(err) = super::types::parse_declarator_suffixes(p)
        {
            return Err(err);
        }

        Ok(CompleteMarker::empty())
    });

    match ended {
        Err(err) => {
            p.close_marks_above(base);
            return Err(err);
        }
        // `true` means the branch's own `;` was the declaration's, so there is nothing left to ask for.
        Ok(ended) if !ended => {
            if let Err(err) = expect_semicolon(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        Ok(_) => {}
    }

    Ok(m.complete(p))
}

/// **One definition per branch**: `#if … = X; #else … = Y; #endif`.
///
/// Two constructs are written this way in libstdc++, and in both the *name* is read before any of it — so the
/// directive arrives between the name and its `=`, which is a position where nothing else can stand:
///
/// ```cpp
/// template<typename _Tp, _Tp _Num>
///   using make_integer_sequence                     // bits/utility.h:174
/// #if __has_builtin(__make_integer_seq)
///       = __make_integer_seq<integer_sequence, _Tp, _Num>;
/// #else
///       = integer_sequence<_Tp, __integer_pack(_Num)...>;
/// #endif
///
/// template<typename _Tp>
///   concept __is_signed_int128                      // bits/iterator_concepts.h:615
/// #if __SIZEOF_INT128__
///       = same_as<_Tp, __int128>;
/// #else
///       = false;
/// #endif
/// ```
///
/// `payload` reads everything after the `=` **except** the `;`, which is the branch's own. The return value says
/// whether that `;` was consumed here: it is when a definition was read and ended with one, and the caller then
/// has nothing left to ask for — while a declaration that never reached a `=` still needs its `;` reported the
/// ordinary way.
fn parse_a_definition_per_branch(
    p: &mut CppParser,
    mut payload: impl FnMut(&mut CppParser) -> ParseResult,
) -> Result<bool, CppParseError> {
    let mut ended_with_a_semicolon = false;

    loop {
        // A directive **before** the `=`, and the one that closes a branch and opens the next.
        while p.current_token() == CppTokenKind::Hash {
            super::stats::parse_preprocessor_directive(p)?;
        }

        if p.current_token() != CppTokenKind::Assign {
            break;
        }
        p.bump();

        payload(p)?;

        if p.current_token() != CppTokenKind::Semicolon {
            ended_with_a_semicolon = false;
            break;
        }
        p.bump();
        ended_with_a_semicolon = true;
    }

    Ok(ended_with_a_semicolon)
}

/// Parse a `typedef` declaration: `typedef int MyInt;`, `typedef WCHAR *PWCHAR, *LPWCH;`.
///
/// # The list, and why it is not one declarator
///
/// A `typedef` is a declaration specifier, so what follows it is an ordinary **init-declarator-list**: C and C++
/// headers introduce several names in one line everywhere, and the pointer aliases in particular are always
/// written this way:
///
/// ```cpp
/// typedef WCHAR *PWCHAR, *LPWCH, *PWCH;      winnt.h, and this shape appears hundreds of times in it
/// typedef struct _GUID *LPGUID, GUID, *PGUID;
/// ```
///
/// This rule read exactly **one** declarator and then insisted on `;`, so every one of those lines failed at the
/// comma. The cost was not the line: the failure left the rest of the declaration to the recovery, which is where
/// `winnt.h`'s 417 errors came from — and, through them, the loss of eight `#endif`s that made the whole file's
/// conditional nesting unusable. One loop.
///
/// Each declarator introduces its **own** name, and each is recorded as a type name: without that, `PWCHAR p;`
/// later in the file reads as an expression rather than as a declaration.
pub fn parse_typedef_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypedefDecl);

    expect_token(p, CppTokenKind::TypedefKeyword)?;

    // The rest is a declaration without the keyword, so reuse the same machinery.
    if let Err(err) = super::types::parse_decl_specifier_seq(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    loop {
        // `typedef int Integer;` makes `Integer` a type name, which is the whole point of the declaration — and
        // the name is what the declarator introduces, so it is read from there rather than from the specifiers.
        // Read off the events the declarator produces, because it is not always the token under the cursor:
        // `*PWCHAR` puts it after a `*`, `(*F)(int)` inside parentheses.
        let declarator_from = p.current_event_count();

        if let Err(err) = super::types::parse_declarator(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        if let Some(name) = p.the_name_a_declarator_introduced(declarator_from) {
            p.declare_type_name(&name);
        }

        // **One initializer for the list, not one per declarator**: `typedef int A, B;` has no `=` at all, and
        // `typedef int *p = nullptr;` is the one shape where an `=` may follow a typedef's declarator — which the
        // ordinary declaration rule reads with the same rule, because it is the same grammar.
        if p.current_token() == CppTokenKind::Assign
            && let Err(err) = parse_initializer_clause(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }

        if p.current_token() != CppTokenKind::Comma {
            break;
        }
        p.bump();
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

    // A macro invocation between the name and the `{`. See the rule for why this is read by shape.
    if p.current_token() != CppTokenKind::LeftBrace {
        eat_namespace_head_macros(p);
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

/// Read the macro-shaped tokens that may sit between a namespace's name and its `{`.
///
/// `namespace std _GLIBCXX_VISIBILITY(default) {` is how **every** libstdc++ header opens — the measured closure
/// has it in one header after another, and it is the first thing in the file — and `_GLIBCXX_VISIBILITY(V)`
/// expands to *nothing at all* on the toolchain this was measured on (`bits/c++config.h` defines
/// `_GLIBCXX_PSEUDO_VISIBILITY(V)` empty there; it is `__attribute__((__visibility__("default")))` only where the
/// toolchain has visibility attributes). So the head the compiler sees is `namespace std {`, and what stands
/// between the name and the brace is a macro invocation. Not reading it cost *the whole file*: the head failed,
/// and every declaration after it came out as an error node — 190 diagnostics in `bits/stl_algobase.h` alone.
///
/// # Why by shape, and not by a table
///
/// The name is `#define`d in `c++config.h`, an **included** header, so nothing the parser can be handed knows it:
/// this file's own `MacroNames` never saw the definition, and the external hook (`symbols.rs`) is not wired to a
/// file's includes — see `docs/std-library.md`, where that connection is called out as the expensive layer it is.
/// What *is* knowable here is the shape, and the shape settles it: **no valid C++ has anything between a
/// namespace's name and its `{`** — the two readings are `namespace std {` and an error — so accepting a macro
/// costs nothing, which is maintenance convention 16's fallback side of the rule ("both readings are wrong, pick
/// the cheaper one").
///
/// # What it does not accept
///
/// A name, or a name and one balanced group, and nothing else. And the scan is **abandoned unless it ends on
/// `{`**: a head that does not close where this expects gives every token back, and the declaration rule then
/// reports exactly what it reported before. That is what keeps a typo — `namespace std ;` — from being read as a
/// namespace with a macro in it.
fn eat_namespace_head_macros(p: &mut CppParser) -> bool {
    let checkpoint = p.checkpoint();
    let mut ate_anything = false;

    while p.current_token() == CppTokenKind::Identifier {
        p.bump();
        ate_anything = true;

        if p.current_token() == CppTokenKind::LeftParen {
            // The group is the invocation's argument list. The same reader a class-member macro call uses, so the
            // two cannot come to disagree about where an invocation ends.
            if parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_err() {
                p.rollback(checkpoint);
                return false;
            }
        }
    }

    if !ate_anything || p.current_token() != CppTokenKind::LeftBrace {
        p.rollback(checkpoint);
        return false;
    }

    true
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
///
/// # Why it does not enter a scope, although every other brace does
///
/// A linkage specification is **not** a scope for names — C++ says the declarations inside it are visible
/// outside exactly as if the block were not there, and only their *language linkage* is affected. So the two
/// things every other braced construct does here are wrong for it, and it does neither:
///
/// * no name scope, so a type declared inside is a type name afterwards (what the standard says);
/// * no *body*, which is what `is_inside_a_body` reports — and this one had teeth. Entering a scope made the
///   parser believe it was inside a body, and the macro-from-a-header rules (`at_a_macro_that_stands_for_a_
///   declaration`, `a_macro_definition_follows`) refuse inside a body by design. Most of libstdc++ is written
///   inside `extern "C++" { namespace std { … } }`, so `_GLIBCXX_BEGIN_NAMESPACE_VERSION` was read as an ordinary
///   name and the declaration after it as an expression: the first error of `cwchar` was
///   ``expected `;` after expression`` at `using ::wint_t;`, and `cstdlib` failed the same way at `using ::div_t;`.
pub fn parse_linkage_block(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    expect_token(p, CppTokenKind::LeftBrace)?;

    // A *block* rather than a class body: `extern "C" { int bits : 3; }` is not a member declaration, and the
    // innermost brace is what the bit-field rule asks about. That question is about the brace, not about scopes,
    // which is why this one stays.
    p.enter_block_body();

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        // **A directive is not a declaration**, and this loop has to say so. Every C header in the world writes
        // its linkage block with the conditional *inside* it:
        //
        // ```cpp
        // #ifdef __cplusplus
        // extern "C" {
        // #endif
        // …
        // #ifdef __cplusplus
        // }
        // #endif
        // ```
        //
        // which is `winnt.h`'s shape at line 11 and every MinGW header's after it. Without this branch the `#endif`
        // went to `parse_declaration`, failed, got wrapped in an `ErrorNode` **one token wide**, and `endif` was
        // then read as the next declaration's *type name*: `endif int x;` is a declaration of `x` with type
        // `endif int`. From there nothing could close the block — the `}` that ends the linkage was consumed by
        // some later declaration — so the loop ran to the end of the file: one `CompoundStat` covering 387 000
        // bytes of `winnt.h`, every directive inside it an `ErrorNode`, and *eight* `#endif`s that the directive
        // scanner never saw. Measured on the 455-file closure: this one construct is why the whole file's
        // conditional nesting was unusable, which is what the branch rule needs.
        if p.current_token() == CppTokenKind::Hash {
            // A directive that does not read is not a reason to lose the block.
            let directive_base = p.open_marks();
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                p.end_marks_to(directive_base);
                p.push_error(err);
            }
            continue;
        }

        let member_base = p.open_marks();
        let before = p.current_token_index();
        if parse_declaration(p).is_err() {
            // **With their end events**, for the third time in this file's history (see `end_marks_to` and
            // maintenance convention 34): a declaration that fails *after* consuming tokens — and inside a linkage
            // block there are hundreds of them — would otherwise leave an unpaired `NodeStart` that the tree
            // builder balances at the **end of the stream**. The price is not one declaration: the failed one
            // swallows every declaration after it, which swallows the `}` that closes the linkage block, which
            // swallows the rest of the file. `winnt.h` came out as sixty nested `TypedefDecl`s all ending at EOF,
            // and with them the whole file's directive structure — which is what made its conditional nesting
            // unusable for the macro layer.
            p.end_marks_to(member_base);
            // Always advance: a declaration that consumed nothing would spin this loop forever.
            if p.current_token_index() == before {
                let error = p.mark(CppSyntaxKind::ErrorNode);
                p.bump();
                error.complete(p);
            }
        }
    }

    p.leave_block_body();

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


