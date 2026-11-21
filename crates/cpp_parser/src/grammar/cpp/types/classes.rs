//! **Classes** — class-like heads: `struct`/`class`/`union`/`enum`, their base clauses, and an enumerator body.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Parse a class, struct, union or enum head: keyword, optional name, optional base clause, optional
/// body.
///
/// This is a *single decl-specifier* rather than a whole declaration, because `class Foo { ... } x;`
/// is legal (if unusual): the head is a type specifier and `x` is the declarator. Keeping the body
/// inside the specifier and letting the declarator follow covers both spellings.
pub(crate) fn parse_class_like_head(p: &mut CppParser) -> ParseResult {
    let base_marks = p.open_marks();
    let keyword = p.current_token();
    let m = p.mark(class_like_kind(keyword).unwrap_or(CppSyntaxKind::ClassDef));

    p.bump(); // `class` / `struct` / `union` / `enum`

    // **Attributes between the class-key and the name**, which is where MSVC's standard library puts them:
    //
    // ```cpp
    // class [[nodiscard]] exception { … };          // <exception>
    // class [[nodiscard]] bad_alloc : public exception { … };
    // ```
    //
    // Read here rather than with the attributes after the name (below), because the name is what this rule reads
    // next: with the attribute in front of it, `p.current_token()` is `[` and the name lookup finds nothing, so the
    // class is recorded as *anonymous* and the body is then read as a compound statement at file scope. Measured on
    // MSVC's `<xstring>` closure: every `class [[nodiscard]] X` lost its name and its body, and the `{` that was
    // read as a function body left a scope open — which the fence then reported as a brace crossing and answered by
    // quarantining four headers, including `<type_traits>` and `<xmemory>`. What that cost was not those classes:
    // it was the whole translation unit, so `std::string` had no declaration anywhere in a project that included
    // `<string>`.
    //
    // Two spellings of one construct, and both are legal; a class may also carry attributes in both places at once
    // (`class [[a]] X [[b]] { }`), which is why this does not replace the loop below.
    while p.current_token() == CppTokenKind::LeftBracket
        && p.peek_next_token() == CppTokenKind::LeftBracket
    {
        if parse_attribute_specifier(p).is_err() {
            break;
        }
    }

    // **`alignas(N)` between the class-key and the name**: `class alignas(2 * sizeof(void*)) _Atomic_ptr_base { … }`,
    // which is where MSVC's `<memory>` puts it. Left alone the head had no name — the class was recorded as
    // anonymous, so its constructors (`_Atomic_ptr_base() noexcept = default;`) were declarations with no return
    // type and each one became an `ErrorNode`.
    while p.current_token() == CppTokenKind::AlignasKeyword
        && p.peek_next_token() == CppTokenKind::LeftParen
    {
        let alignas = p.mark(CppSyntaxKind::AlignasSpec);
        p.bump(); // `alignas`
        if super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_err()
        {
            alignas.undo(p);
            break;
        }
        alignas.complete(p);
    }

    // A **macro between the class-key and the name**, which is where a compiler's alignment attribute is written:
    //
    // ```cpp
    // typedef struct DECLSPEC_ALIGN (8) _XSAVE_AREA_HEADER { … } XSAVE_AREA_HEADER, *PXSAVE_AREA_HEADER;
    //         ^^^^^^ ^^^^^^^^^^^^^^ (8)  the macro and its argument list, then the tag
    // ```
    //
    // `winnt.h` writes this shape for every aligned structure, and the same position holds
    // `__attribute__((packed))` and `__declspec(align(8))`. The macro is defined in `_mingw.h` — *another file* —
    // so the file-local macro table cannot know it, and no spelling convention would be evidence here. What makes
    // accepting it free is the shape: after a class-key the grammar allows an attribute-specifier-sequence, a
    // name, `{`, `:` or `;`, and a **name followed by a parenthesised group** is none of those — there is no other
    // reading to take away. The guard is what follows: this only fires when the run ends somewhere a class head
    // can continue, so a genuine mistake is still a mistake. The same rule the namespace head uses for
    // `namespace std _GLIBCXX_VISIBILITY(default) {` — see `eat_namespace_head_macros`.
    while p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::LeftParen
    {
        let checkpoint = p.checkpoint();
        p.bump(); // the macro's name

        let group_is_read =
            super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_ok();
        let continues = matches!(
            p.current_token(),
            CppTokenKind::Identifier
                | CppTokenKind::LeftBrace
                | CppTokenKind::Colon
                | CppTokenKind::Semicolon
        );

        if !group_is_read || !continues {
            p.rollback(checkpoint);
            break;
        }
    }

    // …and the same position holds a **bare** macro with no argument list: `class _GLIBCXX17_DEPRECATED
    // unary_negate : public …` (`bits/stl_function.h:1021`), where the macro is the standard library's
    // deprecation marker and the name follows it.
    //
    // Two names in a row is not a class head in any reading — after the class-key the grammar allows an
    // attribute, one name, `{`, `:` or `;` — so the first name is the macro. Two guards keep the shapes that
    // *look* similar out of it, and both were needed:
    //
    // * the second name must be followed by something a class head continues with (`{`, `:`, `;`, a directive),
    //   which is what refuses `struct S requires C<T> { }` — there the second name is followed by another name;
    // * `final`, `override` and `requires` are **spellings** rather than tokens (C++11 made the first two
    //   contextual and C++20 the third), so a second name spelled that way is never the class's name here:
    //   `class A final : B` is a class named `A`.
    if p.current_token() == CppTokenKind::Identifier {
        let second = p.peek_token_kind_at(1..2);
        let after_second = p.peek_token_kind_at(2..3);

        let second_is_a_name = second.first() == Some(&CppTokenKind::Identifier)
            && !matches!(p.peek_token_text_at(1), "final" | "override" | "requires");
        let the_head_continues = matches!(
            after_second.first(),
            Some(&CppTokenKind::LeftBrace)
                | Some(&CppTokenKind::Colon)
                | Some(&CppTokenKind::Semicolon)
                | Some(&CppTokenKind::Hash)
        );

        if second_is_a_name && the_head_continues {
            p.bump(); // the macro's name; the class's own name is read below
        }
    }

    // An optional name. `enum class` is handled before this is reached.
    // The name is read before it is parsed, because the parser needs it afterwards and re-deriving it from the
    // event stream would be a second implementation of "what did that name say".
    let declared_name = if p.current_token() == CppTokenKind::Identifier {
        Some(p.current_token_text().to_string())
    } else {
        None
    };

    if matches!(
        p.current_token(),
        CppTokenKind::Identifier | CppTokenKind::Scope
    ) && let Err(err) = parse_name(p)
    {
        p.close_marks_above(base_marks);
        return Err(err);
    }

    // `class Widget { ... }` declares `Widget` to be a type, and the parser needs to know that to read
    // `Widget w(1, 2);` as a declaration rather than as a call.
    //
    // Taken from the **first identifier** of the name, which is the entity being declared: `class ns::Widget`
    // declares `Widget`, and that is what a later declaration spells. A qualified head is therefore recorded
    // under the name it introduces rather than under its qualifier, which is what makes the lookup useful —
    // `Widget w(1, 2);` never mentions `ns`.
    if let Some(name) = declared_name {
        p.declare_type_name(&name);
        // **And what the class body being opened is called**, because a constructor's name *is* its class's name and
        // nothing in the body says so: `Widget(int);` and `Other(int);` are the same three tokens. The body's own
        // doc (see `parse_constructor_declaration`) records what was read without it — a member function whose
        // return type was written in another branch of an `#if` was reached as an identifier followed by `(`, and a
        // rule keyed on that shape renamed the member.
        //
        // Recorded here rather than at the body's `{`, because the name is known here and the body may be opened
        // from a dozen places (a macro between the head and the brace is one of them).
        p.note_class_head_name(&name);
    }

    // Attributes on the class head: `class C [[deprecated]] { ... }`. They may also be written before
    // the keyword, and that form is consumed as a leading decl-specifier by the caller — so both
    // spellings reach the body, and skipping this one makes the class look as though it ended at the
    // attribute. `enum class E [[deprecated]]` already worked, which is exactly why the gap here was
    // easy to miss.
    while p.current_token() == CppTokenKind::LeftBracket
        && p.peek_next_token() == CppTokenKind::LeftBracket
    {
        if parse_attribute_specifier(p).is_err() {
            break;
        }
    }

    // `class D final : public B` — the virt-specifier of a class head. It is written between the name and the
    // base clause, and it is an *identifier* to the lexer (C++11 made `final` and `override` contextual), so
    // no token kind distinguishes it. Without this the `:` after it is never seen as a base clause — the head
    // has ended, the declaration looks for a `;` and finds `:`, and the class body is then read as a compound
    // statement at file scope.
    //
    // Accepted wherever an identifier with that spelling appears in this position, which is the whole test:
    // a class can also be *named* `final` (`class final { };`), and that spelling is claimed before this by
    // the name rule above.
    if keyword != CppTokenKind::EnumKeyword
        && p.current_token() == CppTokenKind::Identifier
        && matches!(p.current_token_text(), "final" | "override")
    {
        p.bump();
    }

    // An enum's underlying type: `enum E : unsigned char { ... }`.
    if keyword == CppTokenKind::EnumKeyword && p.current_token() == CppTokenKind::Colon {
        p.bump();
        if let Err(err) = parse_type_id(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }
    }

    // A base clause: `class D : public B, virtual C`. Only for classes, and only if a `{` really
    // follows — otherwise this `:` is something else entirely (a bit-field, a label).
    //
    // A **directive between the name and the base clause** is read first, and the two are one loop because the
    // two orders occur: the head's base clause may be written inside a branch, or the whole head may be written
    // in two branches that each carry their own `:`:
    //
    // ```cpp
    // class move_iterator                                  // bits/stl_iterator.h:1435
    // #ifdef __glibcxx_ranges
    //   : public __detail::__move_iter_cat<_Iterator>
    // #endif
    // { … };
    // ```
    //
    // Without this the head ended at the name, the declaration asked for a `;`, and the error landed on the `:`
    // of the next line — a diagnostic about a line that is not wrong. The directives are read as the nodes they
    // are, so the tree keeps them and both branches stay visible.
    loop {
        let mut read_a_directive = false;

        while p.current_token() == CppTokenKind::Hash {
            let checkpoint = p.checkpoint();

            if super::super::stats::parse_preprocessor_directive(p).is_err() {
                p.rollback(checkpoint);
                break;
            }

            read_a_directive = true;
        }

        if keyword != CppTokenKind::EnumKeyword
            && p.current_token() == CppTokenKind::Colon
            && a_brace_follows_the_base_clause(p)
        {
            if let Err(err) = parse_base_clause(p) {
                p.close_marks_above(base_marks);
                return Err(err);
            }

            continue;
        }

        // Nothing else to read: the head is as long as it is.
        if !read_a_directive {
            break;
        }
    }

    // The body.
    if p.current_token() == CppTokenKind::LeftBrace {
        let body = if keyword == CppTokenKind::EnumKeyword {
            parse_enumerator_body(p)
        } else {
            super::super::decls::parse_class_body(p)
        };
        if let Err(err) = body {
            p.close_marks_above(base_marks);
            return Err(err);
        }
        // A class definition ends with `;`. It is consumed by the declaration, because
        // `class Foo {} x;` has a declarator after the body.
        //
        // **…and the declaration does not always get the chance to say it is missing.** When what follows the body
        // can begin a declaration of its own — `struct S { int a; } int g;` — the outer reading takes it for a
        // continuation of *this* declaration and no `;` is ever asked for, so the whole file was read as one
        // declaration and everything after it was lost **with no diagnostic at all**. Measured on a reader's own
        // file, one missing semicolon took it from **8 declarations to 2**, and the two symptoms reported from it —
        // no completion for a class in the current file, and none for a variable declared below it — are that one
        // defect seen twice. What settles it is the **spelling**: a class definition's `;` may be followed by a
        // declarator (`x`, `*p`), and a declaration-specifier there cannot continue this declaration.
        if a_declaration_cannot_follow_here(p) {
            p.close_marks_above(base_marks);
            return Err(CppParseError::syntax_error_from(
                &t!("expected `;` after a class definition"),
                p.current_token_range(),
            ));
        }
    }

    Ok(m.complete(p))
}

/// **Is the token at the cursor one that cannot follow a class definition's body?**
///
/// A class definition may be followed by a `;` or by a declarator (`struct S { … } x;`, `enum E { … } *p;`), and
/// those readings are left alone. What this answers is the residue: a token that **begins a declaration** of its
/// own, closes the enclosing scope, or ends the file. In each of those the `;` can only have been forgotten, and
/// saying so is what gets the reader the declarations that follow instead of losing them silently.
///
/// An identifier is deliberately **not** here — that is the declarator reading — and neither is `(` or `[`, which
/// begin one.
pub(crate) fn a_declaration_cannot_follow_here(p: &CppParser) -> bool {
    use CppTokenKind::*;

    matches!(
        p.current_token(),
        RightBrace
            | None
            | StructKeyword
            | ClassKeyword
            | UnionKeyword
            | EnumKeyword
            | TemplateKeyword
            | NamespaceKeyword
            | UsingKeyword
            | TypedefKeyword
            | StaticKeyword
            | InlineKeyword
            | ExternKeyword
            | ConstexprKeyword
            | ConstevalKeyword
            | ConstinitKeyword
            | FriendKeyword
            | VirtualKeyword
            | PublicKeyword
            | PrivateKeyword
            | ProtectedKeyword
            | VoidKeyword
            | AutoKeyword
            | CharKeyword
            | IntKeyword
            | FloatKeyword
            | DoubleKeyword
    )
}

/// Does a `{` appear before the next `;` or `}`?
///
/// Used to tell a base clause (`class D : public B {`) from a bit-field (`int x : 3;`) without
/// lookahead support in the grammar itself.
pub(crate) fn a_brace_follows_the_base_clause(p: &CppParser) -> bool {
    a_body_follows_the_class_head(p)
}

/// Does a class-like head at the cursor actually open a body?
///
/// This is the difference between `class Foo { ... };` and `template <class T> ...`: both start with
/// the `class` keyword, and only the first has a body. It is answered by looking ahead for a `{`
/// that no `;` or `}` intervenes — which is exactly what "the head is followed by a definition"
/// means.
///
/// # Why a head inside a directive ends the question
///
/// Because a conditional can hold **another head for the same declaration**, and then the `{` belongs to that one:
///
/// ```cpp
/// template<typename _Tp, typename _Up>                                   // bits/alloc_traits.h:72
/// #if __cpp_concepts
///   requires requires { typename _Tp::template rebind<_Up>::other; }
///   struct __rebind<_Tp, _Up>                    // ← this head has no body: the other branch does
/// #else
///   struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>
/// #endif
///   { using type = …; };
/// ```
///
/// Reading through the directive, the first head saw the second branch's `{` and took it for its own body: the
/// body rule was entered with a `#` at the cursor, failed, and the whole declaration came apart — `bits/alloc_traits.h`
/// lost `__allocator_traits_base` and everything after it. A class-like keyword *after a directive* is what says
/// "the head this body belongs to is not me", and it is the only thing that does: a base clause may also live
/// inside a conditional (`struct S #if X : public B #endif { }`), and there the `{` really is this head's.
pub(crate) fn a_body_follows_the_class_head(p: &CppParser) -> bool {
    let mut depth = 0isize;
    // A **braced-init-list inside the arguments** is not the body, and this is the third place the same question
    // had to be asked — after the two template-id scans above. The head
    //
    // ```cpp
    // template<typename _Alloc>
    //   struct __is_allocator<_Alloc, __void_t<…, decltype(std::declval<_Alloc&>().allocate(size_t{}))>>
    //   : true_type { };
    // ```
    //
    // writes a `{` **and a `}`** while the head is still going, and the `}` ended the scan: the class was read as
    // body-less, so the base clause and the body became rubble and the diagnostic landed at the end of the line
    // (`bits/alloc_traits.h:941`). Counted the way the angles are — and a `}` that closes nothing still ends the
    // scan, because that one is the enclosing body's own.
    let mut braces = 0isize;
    // Matched parentheses, for the same reason `braces` is counted: `(A < B ? B : A)` holds a `<` that is a
    // comparison and not the start of a template-argument list — see the arm that reads it below.
    let mut parens = 0isize;
    let mut after_a_directive = false;
    // The token before the one being looked at, for the `<` question [`angle_depth_delta`] answers. The scan
    // starts at the head's own `:`, which cannot end a template-name, so that is what it starts as.
    let mut previous = CppTokenKind::Colon;

    // **No fixed window.** What stood here was `p.peek_token_kind_at(0..64)` — a bound on how long a class head
    // may be, which is not a property this rule is entitled to, and the bound was reached: `tuple`'s
    // `_Tuple_cat2` partial specialisation writes a six-parameter head and a five-argument base clause, and the
    // caller that asks from the **`struct` keyword** (rather than from the `:` it is looking for) ran out three
    // tokens before the `{`. The base clause was therefore never read, and the diagnostic landed on the head's
    // own `:` — ``expected `;` `` at a token that is perfectly good C++.
    //
    // The scan ends on the token that decides it and on nothing else: a `{`, a `;`, a `}`, an EOF — and the
    // padding `peek_token_kind_at` appends at end of input is the same EOF answer.
    let mut offset = 0usize;

    loop {
        let Some(kind) = p
            .peek_token_kind_at(offset..offset + 1)
            .first()
            .copied()
            .filter(|kind| *kind != CppTokenKind::None)
        else {
            return false;
        };

        if after_a_directive && is_class_like_keyword(kind) {
            return false;
        }

        match kind {
            CppTokenKind::LeftBrace if depth <= 0 => return true,
            CppTokenKind::LeftBrace => braces += 1,
            CppTokenKind::RightBrace if braces > 0 => braces -= 1,
            // **A `;` inside a braced group is not the head's end.** The same "a matched group hides the
            // structural boundary" rule the angle scan states: `type_traits:3946` writes a class head whose base
            // clause carries a *requires-expression*, and the `;` in its body ended this scan — so the head was
            // read as body-less, the base clause became rubble, and the diagnostic landed on the `:` of a head
            // that is perfectly good C++20.
            CppTokenKind::Semicolon if braces > 0 => {}
            CppTokenKind::Semicolon | CppTokenKind::RightBrace | CppTokenKind::Eof => return false,
            // The `#` of a directive: everything past it is on another line, and may be another branch.
            CppTokenKind::Hash => after_a_directive = true,
            CppTokenKind::LeftParen => parens += 1,
            CppTokenKind::RightParen => parens = (parens - 1).max(0),
            // **A `<` inside matched parentheses is not an angle bracket**, and counting it as one is what made
            // this scan lose a head whose base clause holds a *comparison*:
            //
            // ```cpp
            // template <size_t _First, size_t _Second, size_t... _Rest>
            // struct _Maximum<_First, _Second, _Rest...>
            //     : _Maximum<(_First < _Second ? _Second : _First), _Rest...>::type { … }   // type_traits:1108
            // ```
            //
            // The `<` of `_First < _Second` opened a depth nothing closed, so the head's own `{` was seen at depth
            // 1 — not "the body" — the scan ran on to the `;`, answered **false**, and the base clause was never
            // read: ``expected `;` `` at the head's own `:`. This is the "a matched group hides the structural
            // boundary" rule the braced case above and the parenthesised case in `a_matching_angle_bracket_follows`
            // are both written for.
            _ if parens > 0 => {}
            kind => depth += angle_depth_delta(previous, kind),
        }

        previous = kind;
        offset += 1;
    }
}

/// Parse a base clause: `: public B, virtual private C<T>`.
pub(crate) fn parse_base_clause(p: &mut CppParser) -> ParseResult {
    let base_marks = p.open_marks();

    expect_token(p, CppTokenKind::Colon)?;

    loop {
        let m = p.mark(CppSyntaxKind::BaseSpecifier);

        // Access specifier and `virtual` may appear in either order.
        while matches!(
            p.current_token(),
            CppTokenKind::PublicKeyword
                | CppTokenKind::PrivateKeyword
                | CppTokenKind::ProtectedKeyword
                | CppTokenKind::VirtualKeyword
        ) {
            p.bump();
        }

        // A base is a **class-or-decltype**: a name, or a `decltype`-specifier. The second spelling is the one
        // every `__or_`/`__and_` in libstdc++ uses to name a base it computes rather than writes —
        //
        // ```cpp
        //   template<typename... _Bn>
        //     struct __or_
        //     : decltype(__detail::__or_fn<_Bn...>(0))     // type_traits:199
        //     { };
        // ```
        //
        // — and reading only a name reported `expected a name` against the `:`, which took the whole class head
        // with it. Nothing else about the clause changes: what follows the type is still either a pack expansion
        // or the end of this base.
        if p.current_token() == CppTokenKind::DecltypeKeyword {
            if let Err(err) = parse_type_id(p) {
                p.close_marks_above(base_marks);
                return Err(err);
            }
        } else if let Err(err) = parse_name(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }

        // **A template-id base is one base.** `parse_name` reads `Base` and stops at the `<`, so without this the
        // rest of a base clause — `<T, U>` — was read as further bases: a class with one templated base reported
        // three, and the two "bases" were the template's own arguments.
        //
        // Measured on `std::vector` in libstdc++ (`class vector : protected _Vector_base<_Tp, _Alloc>`): the base
        // list came back as `_Vector_base`, `_Tp`, `_Alloc`, and `members_of` walked three bases — each one an
        // `index.definition` and a `direct_members` — for a class that inherits from one. With the raw summaries a
        // standard-library class has, that walk was **97% of a member query** (583 ms of 600 ms) and the members it
        // was looking for were never there.
        if p.current_token() == CppTokenKind::Less
            && let Err(err) = parse_template_argument_list(p)
        {
            p.close_marks_above(base_marks);
            return Err(err);
        }

        // A pack expansion: `class D : Bases...`.
        if p.current_token() == CppTokenKind::Ellipsis {
            p.bump();
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

/// Parse an enumerator list: `{ Red, Green = 2, Blue }`.
pub(crate) fn parse_enumerator_body(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    expect_token(p, CppTokenKind::LeftBrace)?;

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        // A **directive between enumerators**, which is the same seam as the one in the parameter list, the
        // template parameter list and the specifier sequence, and it is written the same way:
        //
        // ```cpp
        //       bad_file_descriptor = EBADF,
        // #ifdef EBADMSG
        //       bad_message = EBADMSG,
        // #endif
        //       broken_pipe = EPIPE,              // x86_64-w64-mingw32/bits/error_constants.h:52
        // ```
        //
        // A `#` where an enumerator should be is not an enumerator, so the directive is read as the node it is and
        // the question is asked again about the token that follows it. The seam is **before** the enumerator and
        // after the comma, so it also covers a directive that closes a branch in the middle of the list.
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        if p.current_token() != CppTokenKind::Identifier {
            break;
        }

        let enumerator = p.mark(CppSyntaxKind::EnumeratorDecl);
        p.bump();
        let mut took_an_argument_list = false;

        // **An enumerator whose name is a macro invocation** — the X-macro spelling every C library writes:
        //
        // ```c
        // typedef enum {
        // #define XX(code, _) UV_ ## code = UV__ ## code,
        //   UV_ERRNO_MAP(XX)
        // #undef XX
        // } uv_errno_t;
        // ```
        //
        // `UV_ERRNO_MAP` is the enumerator's name and `(XX)` is what the macro takes. Without this the `(` ended the
        // enumerator, the loop wanted a `,` or a `}` there, and the whole `typedef enum … } uv_errno_t;` came out as
        // rubble: measured on libuv 1.53.1's `include/uv.h`, the six declarations up to this point were indexed and
        // the file's other **543 vanished** — which is why eleven `undefined-name` findings pointed at a header that
        // declares them (`docs/c-support.md` §7.1).
        //
        // A **balanced group**, not an expression: the arguments of an X-macro are not enumerators, and a rollback
        // leaves the `(` to the ordinary path (an unterminated group is not this shape).
        if p.current_token() == CppTokenKind::LeftParen {
            let checkpoint = p.checkpoint();
            if super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)
                .is_err()
            {
                p.rollback(checkpoint);
            } else {
                took_an_argument_list = true;
            }
        }

        // Attributes on the enumerator: `enum E { A [[deprecated]] = 1, B };`. Written after the name and
        // before the `=`, so the initializer rule below would otherwise meet a `[[` it has no rule for.
        if let Err(err) = parse_attribute_specifiers(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        // **A macro in the same position** (`omp.h:74`), which is the same seam one spelling along — the
        // attribute the compiler is told about by name rather than written out:
        //
        // ```c
        //   omp_proc_bind_master __GOMP_DEPRECATED_5_1
        //     = omp_proc_bind_primary,
        // ```
        //
        // `__GOMP_DEPRECATED_5_1` is `__attribute__((__deprecated__))` — defined in that file, in two branches —
        // and read as a name the enumerator ended at it: the loop then wanted a `,` or a `}` where the `=` stood,
        // and the whole `typedef enum … } omp_proc_bind_t;` came out as rubble from there on.
        //
        // Only a **name** is taken, and only here: after an enumerator's own name the valid tokens are `[[`, `=`,
        // `,`, a directive and `}` — a name is none of them, so there is no other reading for it to compete with.
        // That is the same argument [`super::super::decls::eat_a_macro_suffix`] documents for the declarator's suffixes,
        // and it is the reader used here rather than a second copy of "a macro invocation".
        while super::super::decls::eat_a_macro_suffix(p) {}

        if p.current_token() == CppTokenKind::Assign {
            p.bump();
            // Read **below the comma operator**, because the enumerator list is comma-separated:
            // `enum E { A = 0, B };` is two enumerators, and an initializer reader that took the comma swallowed
            // `B` whole — the tree then held one `EnumeratorDecl` whose initializer was the expression `0, B`, with
            // every token present, no `ErrorNode` and no diagnostic. A consumer asking the enum for its members
            // lost every one of them after the first initialised one, and nothing said so.
            //
            // This is the **same rule the bit-field width follows**, and for the same reason: a rule that spells a
            // comma itself has to opt out of the operator that spells commas. That one was fixed first and this
            // one was left behind, which is how the defect survived a green test suite — the enum examples in
            // `gaps.rs` all happen to have no initializer.
            if let Err(err) = super::super::exprs::parse_assignment_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        enumerator.complete(p);

        // …and the same seam on the **other side of the enumerator**, because a directive may also close a branch
        // before the comma: `b = 2` `#endif` `,` is written as often as the comma-first spelling, and the comma is
        // what the loop below is looking for.
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
        } else if took_an_argument_list && p.current_token() == CppTokenKind::Identifier {
            // **The comma an X-macro's arguments write is not in this file.** `UV_ERRNO_MAP(XX)` is one enumerator
            // here and *many* after expansion, so the token stream has no `,` between it and the enumerator after
            // it — measured on libuv 1.53.1's `include/uv.h`, where breaking here ended the enum body at the macro
            // and left the rest of the file as rubble. Continuing is safe in exactly this case, and only in this
            // one: an enumerator that took an argument list is a macro invocation, and the next name is the next
            // enumerator rather than a suffix of this one.
            continue;
        } else {
            break;
        }
    }

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

// Does the macro written at the cursor supply the **`::` a qualified name starts with**?
//
// The question is asked of the replacement list's **last** token, and it has to be asked of the body rather than
// of the name, because no spelling answers it: `_STD` is three letters that could stand for anything, and
// `#define _STD ::std::` is a fact about a header. A body that ends at a `::` is a nested-name-specifier, so the
// name after the invocation continues the same qualified name — which is what makes `_STD addressof(*p)` one
// expression and `_STD reverse_iterator<iterator>` one type, instead of two names in a row.
//
// **One predicate, two grammars**: the segment loops of [`parse_name`] here and of `parse_primary_expr` in
// `exprs.rs` ask the same question, and the second copy is where the exception gets forgotten. A buffer parsed with
// no include closure gets `None` — nobody says — and keeps today's reading:
// the invocation is a name of its own, and the name after it is the syntax error it looks like.
// **Does the name at the cursor stand for a *nested-name-specifier* — `#define _STD ::std::`?**
//
// **Gone with the macro table**, and this one is worth a note because it had a real effect: `_STD addressof(*p)`
// read as one qualified name when the closure knew what `_STD` expands to. That is the preprocessor's business and
// the cooked stream settles it — `_STD` is replaced by `::std::` before this grammar runs, and there is nothing
// left to ask. See `crate::preprocess::cook`, which is where the substitution happens.
