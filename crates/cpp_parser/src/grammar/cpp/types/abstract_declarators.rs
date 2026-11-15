//! **Abstract declarators** — abstract declarators — the type with no name in it, which is what a type-id and a cast write.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Parse the part of a declarator that has no name: pointers, references and cv-qualifiers.
///
/// e.g.: `*`, `* const`, `&`, `&&`, `* const*`
/// The abstract part of a declarator: pointer and reference operators, each with the cv-qualifiers
/// that belong to *that* operator.
///
/// The node is created lazily, because "no abstract declarator" is the common case rather than an
/// edge case: in `int counter;` there is no pointer, so demanding a node here would put an empty
/// `Declarator` inside every single declarator in the program. `parse_declarator` already opens the
/// `Declarator` node that `int counter;` needs, so an eager marker here produces
/// `Declarator(Declarator(NameExpr))` — a redundant level that also breaks the 1:1 pairing of
/// `NodeStart`/`NodeEnd` events, since the inner one is empty and gets dropped.
///
/// # `name_possible`
///
/// Whether a declarator's *name* may still appear where this is called, which is what decides one reading:
/// `int (int)`. A name may appear in the position a declarator stands in — `void f(int)` — and then those
/// parentheses are a parameter list rather than part of the type, so they are left alone. It may not appear
/// in a type-id, or inside a parenthesised declarator, and then `(int)` *is* a function type.
///
/// The recursive call passes `false`, because an inner declarator's parentheses wrap a declarator and never a
/// parameter list: `(*f)`, `(* const)`, `(**)`.
pub fn parse_abstract_declarator(p: &mut CppParser, name_possible: bool) -> ParseResult {
    parse_abstract_declarator_with(p, name_possible, true)
}

/// [`parse_abstract_declarator`], plus the one decision the `new` path has to answer differently.
///
/// `read_a_parameter_list_as_the_type` is whether a `(` that follows a complete type may still belong to **the
/// type** — either as a function type spelled with its parameter list (`void (int)`) or as a parenthesised
/// abstract declarator (`int (*)(int)`). In an allocation's type-id the answer is **no**: after the type it has
/// read, a `(` is the **initializer** (`new T(::x)`, `new Widget(1, 2)`), and reading it as part of the type
/// swallowed the initializer and the `;` after it. A type that begins with a parenthesis (`new (Widget)(1)`) is
/// claimed before this function runs, so the gate costs the allocation path nothing.
pub fn parse_abstract_declarator_with(
    p: &mut CppParser,
    name_possible: bool,
    read_a_parameter_list_as_the_type: bool,
) -> ParseResult {
    let mut container: Option<Marker> = None;

    /// Open the `Declarator` node on first use, so an abstract declarator that is not there leaves
    /// no node behind.
    macro_rules! container {
        () => {
            *container.get_or_insert_with(|| p.mark(CppSyntaxKind::Declarator))
        };
    }

    // A pointer or reference operator, then any cv-qualifiers belonging to *that* operator.
    // `int * const p` is a const pointer; `const int * p` is a pointer to const. Reading the
    // qualifiers here, before recursing, is what keeps the two apart.
    loop {
        match p.current_token() {
            CppTokenKind::Star => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::PointerType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::Ampersand => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::ReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::LogicalAnd => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::RValueReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            // **A Clang block declarator**: `void (^handler)(void)`, `void (^block)(int) = ^(int n) { … };`.
            //
            // A block is a pointer-shaped thing whose operator is `^` rather than `*` — the grammar of a block
            // declaration is the function-pointer declaration with one token changed, which is why it is read here
            // as the same node a `*` produces. Before this, `void (^block)(int)` was a declaration with no name:
            // the `(` group failed to read as a declarator and the statement fell back to an expression, reporting
            // ``expected `;` after expression`` at the `)`.
            //
            // Only in declarator position: `^` is also the XOR operator, and an expression's `^` never reaches this
            // function (the expression grammar owns it).
            CppTokenKind::Caret => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::PointerType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            // **A pointer to member written after the type**: `int C::*p;`, `int (C::*h)(int);`,
            // `void g(int (C::*)(int));`.
            //
            // The grammar puts the nested-name-specifier *inside* the ptr-operator (`nested-name-specifier *`),
            // so the name that begins it stands where the abstract declarator begins — this loop — and not where
            // the declarator's own name does. Reading it as a type instead is what the silent defect looked like:
            // `C::` joined the specifier sequence, `*p` became a declarator **inside the type node**, and nothing
            // anywhere said so. `continues_a_qualified_name` now refuses a `::` that a `*` follows, which is what
            // brings the tokens here.
            //
            // The parenthesised spelling had one failure mode on top of that one: `int (C::*h)(int);` reaches the
            // declarator with `C` at the cursor, this loop broke immediately on a name, and the parentheses came
            // out empty and the declaration failed — the form an out-of-line member-pointer *typedef* is written
            // in.

            // **A calling convention before the operator** — the abstract declarator's half of the rule that
            // reads `int *__cdecl _errno(void)` and `(__cdecl * _onexit_t)`: `(__cdecl *)` writes the keyword
            // *before* the `*`, and without this arm the loop broke on the name and the parentheses came out
            // empty. The keyword is `Bare` — it says nothing about the type or the name, so it stays the token it
            // was written as — and the loop continues, which is what lets the `*` after it still be read.
            //
            // A directive is what it looks like; a *name written like a macro* is the only thing this accepts,
            // so `(x *)` keeps the reading it had.
            CppTokenKind::Identifier
                if matches!(
                    an_implementation_keyword(p),
                    Some(ImplementationKeyword::Bare)
                ) =>
            {
                let _ = container!();
                p.bump();
            }
            // **An unexpanded macro standing in the pointer operator, before the declarator's name** —
            // `*RESTRICTED_POINTER PRKCRM_MARSHAL_HEADER`, which `shared/ktmtypes.h` writes eleven times in one
            // typedef after another.
            //
            // What the name is: `winnt.h:110` defines it as `__restrict` on the one target that wants the
            // qualifier and as *nothing at all* everywhere else (`#else` → `#define RESTRICTED_POINTER`). So it
            // stands where a **type qualifier** stands, exactly as `__restrict` does in the arm above — and it is
            // a *macro*, not a keyword, so which of the two it is depends on the closure: a reading that does not
            // expand it (the file's own text) or cannot (nobody said what it is) meets a bare name here.
            //
            // The follower is what makes the arm safe, and it is the same requirement the calling-convention
            // rule above documents: `*MACRO name` is told from `*name` by the second name and by nothing else.
            // The ordinary pointer declarator has one name (`int *p;`) and is untouched; `*x y` in broken code
            // stays an error, because `x` is not written the way a macro is.
            //
            // Measured: `ktmtypes.h` reads clean in the file's own text **by accident** — the declarator takes
            // `RESTRICTED_POINTER` as its name, the typedef ends without its `;`, and `PRKCRM_MARSHAL_HEADER ;`
            // is read as a declaration of its own: well formed, lossless, no diagnostic, and two names wrong.
            // Over the cooked stream the same tokens report ``expected `;` `` instead, because there the
            // invented `;` is not available.
            //
            // **An operator has to have been read** (`container.is_some()`), and that is not a formality: the
            // pair this arm fires on is `MACRO name`, which is also the pair inside a parenthesised declarator —
            // `(WINAPI PM_OPEN_PROC)(LPWSTR)`, where `WINAPI` is a `MacroCall` and `PM_OPEN_PROC` is the name.
            // Without the gate this arm claimed the group's first name before that rule could see it, and the
            // assertion that pins the calling-convention shape failed: the `MacroCall` was gone. Here the arm is
            // only reachable *after* a `*`, `&`, `&&` or `::*`, which is the position the macro actually
            // decorates.
            CppTokenKind::Identifier
                if container.is_some()
                    && written_like_a_macro(p.current_token_text())
                    && p.peek_token_kind_at(1..2) == [CppTokenKind::Identifier] =>
            {
                let _ = container!();
                p.bump();
            }
            // …and the **mirror of it**: the macro written *before* the operator, as the old Windows memory-model
            // spelling does — `} MMTIME , * PMMTIME , NEAR * NPMMTIME , FAR * LPMMTIME ;` (`um/mmsyscom.h:138`,
            // and `um/mciapi.h`, `um/mmiscapi.h` and `shared/rpcdcep.h` write the same). `NEAR` and `FAR` come
            // from `minwindef.h:150` (`#define NEAR near`), and `near`/`far` are themselves empty macros two
            // definitions above it — so the compiler reads `* NPMMTIME` and this reading has to as well.
            //
            // The two arms are one concept at two positions, and both need the follower: here it is the **`*`**
            // that must come next, which is what keeps a name that merely *looks* like a macro from being taken —
            // the pair `MACRO *` occurs in no valid declarator, so the only readings are this one and an error.
            CppTokenKind::Identifier
                if written_like_a_macro(p.current_token_text())
                    && matches!(
                        p.peek_token_kind_at(1..2).as_slice(),
                        [CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd]
                    ) =>
            {
                let _ = container!();
                p.bump();
            }
            // …and a **calling convention in front of the class name** — `(__cdecl A::*)`, which is the group
            // `a_parenthesised_abstract_declarator_follows` claims for MSVC's `_IS_MEMFUNPTR` (see its note).
            // The macro decorates the pointer-to-member operator exactly as `__cdecl` decorates a plain `*`
            // (`(void (__cdecl *)(void))`), so it is read the same way — as a token that says nothing about the
            // type — and the loop then reads the `A::*` through the arm below.
            CppTokenKind::Identifier
                if written_like_a_macro(p.current_token_text())
                    && pointer_to_member_operator_length(p, 1).is_some() =>
            {
                let _ = container!();
                p.bump();
            }
            CppTokenKind::Identifier if pointer_to_member_operator_length(p, 0).is_some() => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::PointerType);

                // The nested-name-specifier: `A::B::` is a name and a `::` per segment, and the `::` that ends
                // the specifier is the one the `*` follows.
                while p.peek_next_token() == CppTokenKind::Scope {
                    p.bump(); // a name of the nested-name-specifier
                    p.bump(); // its `::`
                }
                p.bump(); // `*`
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            // `Class::*` — a pointer to member.
            //
            // The node is only opened once the `*` has actually been seen: opening it up front and
            // rewinding on failure leaves a registered marker behind, which suppresses the
            // `container!` call on the next iteration and can spin this loop forever on a
            // qualified name.
            CppTokenKind::Scope
                if matches!(
                    p.peek_next_token(),
                    CppTokenKind::Star | CppTokenKind::Identifier
                ) =>
            {
                let checkpoint = p.checkpoint();
                p.bump(); // `::`

                // `Ident :: *` is a member pointer; a bare `::*` cannot occur, so falling through
                // here means this `::` belonged to a qualified name instead.
                if p.current_token() == CppTokenKind::Identifier
                    && p.peek_next_token() == CppTokenKind::Scope
                {
                    p.bump(); // the class name
                    p.bump(); // `::`
                }

                if p.current_token() == CppTokenKind::Star {
                    let _ = container!();
                    let op = p.mark(CppSyntaxKind::PointerType);
                    p.bump();
                    eat_cv_qualifiers(p);
                    op.complete(p);
                } else {
                    p.rollback(checkpoint);
                    break;
                }
            }
            // **A directive inside the run of pointer/reference operators**, which is the same seam as
            // everywhere else in this file and the one that decides how much of `bits/basic_string.h` reads:
            //
            // ```cpp
            // #else                                        // bits/basic_string.h:2631
            //   template<class _InputIterator>
            // #ifdef _GLIBCXX_DISAMBIGUATE_REPLACE_INST
            //     typename __enable_if_not_native_iterator<_InputIterator>::__type
            // #else
            //     basic_string&
            // #endif
            //     replace(iterator __i1, iterator __i2, _InputIterator __k1, _InputIterator __k2)
            // ```
            //
            // One declaration with a conditional *return type*, so the `&` of `basic_string&` is followed by the
            // `#endif` that closes the type's alternation and only then by the declarator's name. The `#ifdef` half
            // is read by the specifier sequence, which is where the other branch's head is read too; this directive
            // is the last token of the construct and had no owner at all — the declarator came out **nameless**,
            // the declaration ended at the `&`, and every member written after it was read as a child of that
            // nameless declarator instead of as a member of the class. Silent, lossless, no diagnostic.
            //
            // Read as the node it is and go round again — the next token is the name, or another operator. A `#`
            // cannot be part of a declarator: nothing in `int * # x` has a reading in any grammar.
            //
            // Only once an operator has been read (`container` is open): a directive *before* the first one is not
            // this rule's, and taking it here would take it away from the loop that owns the declaration's
            // specifiers — the rule that has to see it to keep its own sequence going.
            CppTokenKind::Hash if container.is_some() => {
                super::super::stats::parse_preprocessor_directive(p)?;
            }
            _ => break,
        }
    }

    // A **parenthesised** abstract declarator, or a function type whose parentheses hold the parameter list.
    //
    // The first form is `void (*)(int)`, `int (&)[10]`: the parentheses are what let the `*` bind to the
    // function rather than to its return type, which is the whole reason the syntax exists — `void *f(int)` is
    // a function returning a pointer, `void (*f)(int)` a pointer to a function. A type-id — a `using` alias, a
    // cast, the target of a `sizeof` — has no name to hang the parentheses on, so it reaches this with nothing
    // parsed yet, and without the branch the alias is left with a specifier sequence and three stray tokens.
    //
    // The second is `void (int)`, `int (char, double)` — a function type, spelled with the parameter list
    // directly. It is the same shape as the `(*)(int)` form and reached by *dropping* the pointer, which is
    // how `new (Widget)(1)` writes an allocation whose type is parenthesised. The two are told apart by what
    // the parentheses hold: a type specifier means the list is a parameter list, and anything else means the
    // parentheses wrap a declarator.
    //
    // Whether the parentheses belong here at all is the question {@link
    // a_parenthesised_abstract_declarator_follows} answers, and it is asked *before* the parenthesis is
    // consumed: `int (x)` is a parenthesized declarator for the caller to read, and `void f(int)` has
    // parentheses that are not this rule's business at all.
    // Whether the parentheses belong to this declarator is asked before they are consumed, and the answer
    // depends on something the caller knows and this function does not: whether a **name** may still appear.
    //
    // * `void f(int)` — a name may appear next, so `(int)` is the parameter list of `f`, and it is not this
    //   rule's business at all. Consuming it here would take the whole declaration apart.
    // * `new (int)(1)` — no name may appear, so the `(int)` *is* the type: a function type spelled with its
    //   parameter list directly.
    // * `new (Widget)(1)` — the same, with the parameter list holding a type name.
    //
    // Both readings are the same four tokens, so the decision cannot come from them; it comes from who is
    // asking, which is what `name_possible` carries.
    // **Both arms of this block are the type only where the caller says a `(` may still belong to it.** An
    // allocation's type-id answers no: after the type it has read, a `(` is the **initializer**, and reading it
    // here is what made `new T(::x)` report ``expected ), but get ::`` — the `(::x)` came out as a parenthesised
    // abstract declarator (the first arm), the type-id swallowed the initializer *and* the `;` that followed it,
    // and `::x` was reported as an unexpected `::`. A type that genuinely *begins* with a parenthesis
    // (`new (Widget)(1)`) is claimed before this function runs, by the branch that reads a type beginning with
    // one, so nothing that belongs to a type is lost by the gate.
    if p.current_token() == CppTokenKind::LeftParen && read_a_parameter_list_as_the_type {
        // **A group that holds a *name* is this declarator's, not a type's** — and the case that reaches here is
        // the one where operators were written in front of it:
        //
        // ```c
        // typedef char *(*LineGetter)(int, void *, int, bool);      // neovim's ex_cmds_defs.h:96
        // ```
        //
        // The `*` of `char *` is the type's, the parenthesised `(*LineGetter)` is the declarator's, and this
        // function is entered with the cursor on that first `*` — it reads the operator, then meets a `(` whose
        // group holds a name. Read as an *abstract* declarator (the arm below), the group's `LineGetter` had no
        // reader at all: `expected )` was reported at the name, the declarator was left open, and **the rest of
        // the file became that declarator's tokens** — measured, the `TypedefDecl` for `LineGetter` spanned
        // everything to the end of the header, so `struct exarg` and every declaration after line 96 produced no
        // facts. `members_of("exarg_T")` was empty for the same reason.
        //
        // The predicate is the one the caller uses for the unprefixed spelling (`(*f)(int)` with nothing in front
        // of it), asked here because only now has the operator run been read: a group with a name is a
        // *declarator*, a group without one is a type, and that is the whole distinction.
        if name_possible && a_parenthesised_declarator_with_a_name_follows(p) {
            let _ = container!();
            parse_parenthesised_declarator(p)?;
            parse_declarator_suffixes(p)?;
        } else if a_parenthesised_abstract_declarator_follows(p) {
            let _ = container!();

            expect_token(p, CppTokenKind::LeftParen)?;
            // The inner abstract declarator, recursively: `(* const)`, `(**)`, `(&)`.
            parse_abstract_declarator(p, false)?;
            expect_token(p, CppTokenKind::RightParen)?;

            parse_declarator_suffixes(p)?;
        } else if !name_possible && a_parameter_list_is_the_type(p) {
            let function_type = p.mark(CppSyntaxKind::FunctionType);
            super::super::decls::parse_parameter_list(p)?;
            eat_function_qualifiers(p);
            // The `noexcept` of `void () noexcept` and a trailing return type both belong to the type.
            parse_declarator_suffixes(p)?;
            function_type.complete(p);
        }
    }

    match container {
        Some(m) => Ok(m.complete(p)),
        // No abstract declarator here at all — the caller's own `Declarator` node covers it.
        None => Ok(CompleteMarker::empty()),
    }
}

/// Do the parentheses at the cursor hold a parameter list, making them a *function type*?
///
/// e.g.: the `(int)` of `new (void(int))()` or of `sizeof(void(int))`
///
/// A parameter list begins with a type, so the test is that the first token inside the parentheses can begin
/// one. That is deliberately weaker than "parses as a parameter list" — the caller parses it and reports
/// whatever goes wrong — and it is deliberately *not* satisfied by a pointer or reference operator, because
/// `(*)(int)` is the parenthesised-declarator form and is handled before this is asked.
///
/// A `)` is excluded as well **unless a keyword type stands in front of it**: `void ()` is a function type with no
/// parameters, and it is the one case where the parentheses hold nothing at all, so nothing inside them can
/// answer. The token before them does: a keyword type is a type whatever follows it — `std::function<void()>` is
/// everywhere, and it was read as `void` with a stray `()` after it, which cost the whole argument — while a
/// *name* before an empty group may be a call (`f()`), and stays one: the argument reader falls back to the
/// expression reading for that case, and it is the more useful reading of the two.
pub(crate) fn a_parameter_list_is_the_type(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    let kinds = p.peek_token_kind_at(0..64);

    if kinds.get(1) == Some(&CppTokenKind::RightParen) {
        return p
            .last_consumed_token_kind()
            .is_some_and(is_type_specifier_keyword);
    }

    // **Every** element has to start like a parameter, not just the first one, and that is what tells the two
    // readings of a parenthesised group apart when the group's first token is a name:
    //
    // ```text
    // new T(int, char)   a function type, spelled with its parameter list
    // new T(a, *q)       an allocation of `T` initialised with `(a, *q)` — `*q` is not a parameter
    // ```
    //
    // A parameter is a type and then a declarator, so nothing but a type can follow a `(` or a top-level `,`. The
    // first version of this predicate asked only about the first token, so `a` decided for the whole group: the
    // group was read as a parameter list, `*q` had no type, and the allocation came out as
    // `expected a type specifier` against the `*` — measured in `bits/uses_allocator.h`,
    // `bits/node_handle.h` and `memory_resource.h` of the standard-library closure.
    let mut depth = 0isize;

    for (offset, kind) in kinds.iter().enumerate() {
        let starts_an_element =
            offset == 1 || (depth == 1 && kinds.get(offset - 1) == Some(&CppTokenKind::Comma));

        if starts_an_element && !begins_a_parameter_type(*kind) {
            return false;
        }

        // **A token only an expression has** means the group is an initializer, not a parameter list — and the
        // element-start check above cannot see it, because it only looks at each element's *first* token:
        //
        // ```text
        // ::new (std::__addressof(_M_alloc)) _NodeAlloc(__nh._M_alloc.release());   // bits/node_handle.h:157
        // new T(x + 1)          an allocation initialised with `x + 1`
        // ```
        //
        // `(__nh._M_alloc.release())` begins with a name, so every element started like a parameter, the group was
        // claimed as a *function type*, and the parameter reader then met the `.` and reported `expected ), but get
        // .` — while the parentheses are the allocation's own **initializer**, which is what
        // `parse_new_initializer` reads once the type stops where it should.
        //
        // `-` is deliberately **not** in the list: `= -1` is a default argument, which a parameter list may hold.
        // Nothing here can cost a real parameter list much either way — this predicate is only asked in a type-id,
        // where a parameter has no name and a default argument has no meaning.
        if depth == 1
            && matches!(
                kind,
                CppTokenKind::Dot
                    | CppTokenKind::Arrow
                    | CppTokenKind::Plus
                    | CppTokenKind::Slash
                    | CppTokenKind::Percent
                    | CppTokenKind::Pipe
                    | CppTokenKind::Caret
                    | CppTokenKind::Tilde
                    | CppTokenKind::LogicalOr
                    | CppTokenKind::Equal
                    | CppTokenKind::NotEqual
                    | CppTokenKind::Question
            )
        {
            return false;
        }

        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                depth += 1
            }
            CppTokenKind::RightParen => {
                depth -= 1;

                if depth == 0 {
                    // The group closed, and every element began like a parameter.
                    return true;
                }
            }
            CppTokenKind::RightBracket | CppTokenKind::RightBrace => depth -= 1,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    // Unterminated within the lookahead: not a group this rule can claim.
    false
}

/// Can a **parameter** begin with this token?
///
/// A parameter is a type and then a declarator, so this is the set of tokens a type may begin with — the same set
/// the first element has always been judged by, plus `...` for `void f(...)`, which is a parameter list and
/// nothing else.
pub(crate) fn begins_a_parameter_type(kind: CppTokenKind) -> bool {
    is_type_specifier_keyword(kind)
        || matches!(
            kind,
            CppTokenKind::Identifier
                | CppTokenKind::Scope
                | CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::Ellipsis
        )
}

/// Consume any `const` / `volatile` — or an implementation qualifier — immediately following a pointer,
/// reference or pointer-to-member operator.
///
/// The second half is the same rule as the specifier position, one token later: `int *__cdecl _errno(void);` and
/// `const char * __restrict__ _Src` both write the keyword *after* the `*`, because the `*` belongs to the
/// return or element type. `__cdecl` there is a calling convention rather than a qualifier, and it is accepted
/// here for the same reason: nothing else can stand in this position, and refusing it loses the declaration.
pub(crate) fn eat_cv_qualifiers(p: &mut CppParser) {
    loop {
        if matches!(
            p.current_token(),
            CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
        ) {
            p.bump();
            continue;
        }

        // **C99's `restrict`**, the one spelling this list was missing. The compiler's own `__restrict`/`__restrict__`
        // are [`an_implementation_keyword`]'s; the *standard* spelling is an ordinary identifier, so it stood in the
        // same position with nothing to read it — and `static int restrict_sum(int *restrict left, int *restrict right)`
        // came out as two parameters *named* `restrict`, with `left` and `right` standing after them as macro suffixes.
        // The two then collided and `redefinition` reported the second (measured on a nine-file C corpus, and pinned by
        // `a_restrict_parameter_after_other_declarations_is_not_a_name`). The feature is the language's: C99 and later,
        // or GNU's dialect — a strict C++ file has an ordinary identifier called `restrict`, and says so.
        if p.current_token() == CppTokenKind::Identifier
            && p.current_token_text() == "restrict"
            && p.language().support(crate::features::Feature::Restrict)
        {
            let m = p.mark(CppSyntaxKind::RestrictQual);
            p.bump();
            m.complete(p);
            continue;
        }

        if an_implementation_keyword(p).is_some() {
            parse_an_implementation_keyword(p);
            continue;
        }

        // **An attribute** in the same position, which is the third thing written there and the one this rule was
        // missing:
        //
        // ```cpp
        // extern __inline void * __attribute__((__gnu_inline__, __always_inline__, __artificial__))
        // __slwpcb (void) { … }                      // lwpintrin.h:43 — the whole family of `*intrin.h` files
        // ```
        //
        // The doc above already says this is "the same rule as the specifier position, one token later", and an
        // attribute is one of the things written in the specifier position — so it belongs here for the reason
        // `__cdecl` does: the attribute belongs to the return type, and nothing else can stand between the `*` and
        // the declarator's name (the name cannot be `__attribute__`).
        //
        // What it cost to leave out is not a diagnostic but a **silent wrong tree**: `void * __attribute__((x)) f;`
        // came out as a declarator *named* `__attribute__` and initialised with `(x)`, with `f` standing after it
        // as a macro — lossless, well-formed, no `ErrorNode` — and the same shape with a body reported
        // ``a declarator takes only one initializer`` at the `{`, because the declarator already had one.
        if at_an_attribute(p) {
            let checkpoint = p.checkpoint();
            if parse_attribute_specifier(p).is_err() {
                // An unterminated `[[` is not this shape: give the tokens back and let the declarator report
                // whatever it reported before — the same shape [`super::super::decls::eat_a_macro_suffix`] uses.
                p.rollback(checkpoint);
                return;
            }
            continue;
        }

        // **A macro that expands to nothing** stood here — `#define POINTER_32`, empty on a 64-bit target, used
        // exactly where a pointer qualifier goes. It is gone with the macro table, and the cooked stream is what
        // makes that safe rather than lossy: a name whose expansion is empty leaves **no token at all**, so this
        // position never sees it. See `crate::preprocess::cook`.

        return;
    }
}
