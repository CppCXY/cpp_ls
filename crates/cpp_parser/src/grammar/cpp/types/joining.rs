//! **Joining** — when a name joins the type and when it is the declarator — the question the whole module is about.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// The two ways a name may still become a **word of the type**, computed by the specifier loop and asked about
/// again inside it.
///
/// One value rather than two parameters because they answer the same question in two contexts — "may this name be
/// part of the type?" — and because the specifier loop's signature is already at the limit a reader can hold:
///
/// ```text
/// a_further_name                            a declaration: `MY_API Widget *p`, `unsigned __int64 x;`
/// a_macro_shaped_name_closes_a_type_id      a type-id, where there is no declarator: `(unsigned __LONG32) h`
/// ```
#[derive(Clone, Copy)]
pub(crate) struct NamesMayJoin {
    /// A further name may join a type that is already in the sequence — see [`name_joins_the_type`] for the three
    /// conditions and for the two spellings it decides between (`unsigned __int64 x;` against `int x
    /// MY_DECL_SUFFIX;`).
    pub(crate) a_further_name: bool,
    /// A **macro-shaped** name may close a **type-id**, where there is no declarator to make room for: the cast in
    /// `return ((unsigned __LONG32) (ULONG_PTR) h);`. See [`written_like_a_macro`].
    pub(crate) a_macro_shaped_name_closes_a_type_id: bool,
}

/// May the name at the cursor still be part of the type being parsed, rather than the declarator?
///
/// This is *the* ambiguity of a declaration, and it cannot be settled by looking at one token:
///
/// ```text
/// T x;                    // type `T`, declarator `x`      -> the name joins the type
/// const Point p;          // type `const Point`, `p`       -> the name joins the type
/// std::vector<int> v;     // type `std::vector<int>`, `v`  -> the name is the declarator
/// void f();               // type `void`, declarator `f`   -> the name is the declarator
/// ```
///
/// Three conditions, and each covers a case the others get wrong:
///
/// * `has_type_specifier` — false while nothing has named a *type*, where a lone name is certainly the type
///   (`Foo` in `Foo x`, `T` in `alignas(16) T x`). A specifier that is not a type — `alignas`, `const`,
///   `static` — leaves this false on purpose: it says nothing about the type, so the next name still has room
///   to be one. Reading it as "a specifier was consumed" is what made `alignas(16) MyType value;` take `MyType`
///   for the declarator and leave `value` with nowhere to go.
/// * `continues_a_qualified_name` — the loop walks `std::vector` one segment per iteration and every
///   segment is part of the same name.
/// * `name_allowed` — whether a name may still join. `T x` lets `x` in and then spends the
///   allowance; `void f` never spends it, because `void` is a keyword and a keyword can never be
///   the declarator. That is the whole reason `T x;` and `void f();` can both be right.
/// * `type_is_already_complete` — the sequence so far stands on its own as a type, so a following
///   name must be the declarator even though the allowance is unspent. Without this `void f;` reads
///   `f` as a second word of the type.
/// * `a_parenthesis_follows_the_name` — the name about to join is *called*. No type is written that
///   way, so it is the declarator and the parentheses are what initialises it. See
///   [`a_parenthesis_follows_the_name`] for the whole argument; it is what makes `Widget w(1, 2, 3);`
///   a declaration without asking the type table.
/// * `a_class_definition_was_written` — the sequence has a **body** in it, and a body is a complete type:
///   `struct S { … } x;`, `struct { … } x;`, `enum E { A } e;`, `typedef struct { … } Alias;`. The walk
///   [`type_is_already_complete`] makes cannot see it — it meets the `}` and answers "no type yet", which is the
///   right answer for the other meaning of a `}` — so the name that follows a definition must be the declarator.
pub(crate) fn name_joins_the_type(
    p: &CppParser,
    has_type_specifier: bool,
    name_allowed: bool,
    names_may_join: NamesMayJoin,
    a_class_definition_was_written: bool,
) -> bool {
    // **A qualified name written straight against `(` is the declaration's own name**, not another word of the type:
    //
    // ```cpp
    // int S::f() { return 0; }        "S::f" is the function, and `int` is the whole return type
    // ```
    //
    // In a declaration a **type is never followed by `(`**, so a name that is qualified *and* immediately precedes a
    // parameter list is the declarator. Without this the specifier sequence swallowed the function's own name — the
    // declaration was `DeclSpecifierSeq "int S::f"` with a declarator that named nothing, which meant every
    // out-of-class definition in a project was nameless: not a member of its class, no enclosing class inside its
    // body (the completion defect), and `missing-return` reading `S::f` as a return type. See the dispatch arm in
    // `decls` for the constructor half of the same shape.
    //
    // Asked **before** the "no type yet" early return below, because that is the other half: `S::S(int)` has no type
    // before the name and would otherwise be taken for one.
    if has_type_specifier && crate::grammar::cpp::decls::starts_an_out_of_class_member(p) {
        return false;
    }
    // A **class-like definition** is a complete type, body and all, and the name after it is the declarator:
    //
    // ```text
    // struct S { int a; } x;          declares `x`
    // struct { int a; } x[] = { … };  the C idiom this was found in — an unnamed struct and its variable
    // union { int a; } u = { 1 };     …and the same shape at block scope, which is where it was missed
    // enum E { A } e;                 the same for an enum
    // typedef struct { … } Alias;     the body is the type, `Alias` is the name
    // ```
    //
    // Read the other way the name joined the *type*, so the declaration had no declarator at all: silently for
    // `struct S { … } x;` (a well-formed declaration of nothing, no diagnostic), and loudly as soon as the
    // declarator carried anything — `x = { 1 }` reported `expected a declarator name` against the `=`, and
    // `x[2]` reported `expected ], but get integer literal`. `LuaDefine.h` in the first real C++ project is the
    // third shape, 45 diagnostics from one declaration.
    //
    // **Asked before the "no type yet" early return below**, and that order is what matters: an **anonymous**
    // definition never writes a name, so `has_type_specifier` is still false when the declarator arrives and the
    // early return took it for the type —
    //
    // ```cpp
    // union { __m128h __a[2]; __m256h __v; } __u = { .__v = __A };     // avx512fp16vlintrin.h:155
    // ```
    //
    // — leaving a declaration whose type is `union { … } __u` and which declares nothing. Without an initializer
    // that was **silent** (no diagnostic, no `ErrorNode`, no `MissingNode`); with one it was reported at the `=`,
    // because an initializer needs something to initialise. The body is a complete type whatever the sequence's
    // flags say, so the question is asked first.
    if a_class_definition_was_written {
        return false;
    }
    // No type yet, so this name can only be the type.
    if !has_type_specifier {
        return true;
    }
    // **A name that begins a pointer-to-member operator is not a word of the type**: `A B::*` is the type `A` and the
    // operator `B::*`, never a type named `A B::`. Template arguments are where it shows — `_Ty1 _Ty2::*` in MSVC's
    // `<type_traits>` (`struct _Is_member_object_pointer<_Ty1 _Ty2::*>`), and `int X::*>` for a name spelled like a
    // macro — because a name after a complete type is otherwise taken for another word of it, and the `*` is then
    // left as a declarator inside the type. The failure was silent: no diagnostic, and the declaration that
    // contained it never ended, so everything after it in the file was part of it.
    if has_type_specifier && pointer_to_member_operator_length(p, 0).is_some() {
        return false;
    }
    // A **type-id**, where a macro-shaped name after a type is simply another word of the type and there is no
    // declarator to make room for:
    //
    // ```cpp
    // static __inline unsigned __LONG32 HandleToULong (const void *h)
    // { return ((unsigned __LONG32) (ULONG_PTR) h); }        // basetsd.h:68 — the cast's type-id
    // ```
    //
    // `allow_second_name` is `false` in a type-id, and that is right for the question it was written for (a second
    // *name* would run two template parameters together); what it must not do is refuse a name that is written the
    // way an unexpanded macro is written. The caller has already checked that: `has_type_specifier` says a type is
    // there, [`written_like_a_macro`] says this name is spelled like a macro, and there is no follower question to
    // ask because a type-id has no declarator. Nothing else is relaxed — `-> int requires C<T>` still ends at
    // `int` because `requires` is not written like a macro.
    if names_may_join.a_macro_shaped_name_closes_a_type_id {
        return true;
    }
    // An **elaborated type specifier**: `struct S`, `union U`, `enum class E`. The keyword is not the type — the
    // name after it is — so this name joins the type whatever the caller said about a *second* name:
    //
    // ```text
    // struct S *p;          a declaration, where a second name is allowed
    // sizeof(struct S)      a type-id, where it is not
    // using A = struct S;
    // (struct S *)p;
    // ```
    //
    // In a type-id the allowance had already been spent on the keyword, so `S` was refused and the type-id came
    // out as `struct` on its own: the payload never reached its `)`, and `sizeof(struct S)` — an ordinary thing to
    // write in C — was reported as `expected primary expression`. The keyword and its name are one specifier, so
    // the allowance never applied to this name in the first place.
    if p.last_consumed_token_kind()
        .is_some_and(is_class_like_keyword)
    {
        return true;
    }
    if continues_a_qualified_name(p) {
        return true;
    }
    // A **second name in the specifier sequence**, which is what an export or attribute macro looks like:
    //
    // ```text
    // MY_API Widget *p;          a declaration of `p`
    // EMMY_API RangeResult f();  a declaration of `f`
    // EXPORT std::string g();    the macro, then a qualified type, then the declarator
    // ```
    //
    // A macro is not expanded here, so `MY_API` is an ordinary name to this parser and the type that follows it
    // is a *second* name in the same sequence. The allowance is spent after the first, so the second was refused
    // and became the declarator name, and the real declarator (`p`, `f`) had nowhere to go: the declaration
    // failed and the whole line came back as an expression statement — `expected ;` against the return type. It
    // is how `CodeFormatCLib.cpp` fails, and there is nothing rare about the spelling: every real C++ project
    // with a DLL/export boundary writes it.
    //
    // What makes the name part of the type rather than the declarator is that **the declaration has not reached a
    // declarator yet**: after it come more names (`B C d`), a `*`, a `&`/`&&`, a `::` or a `<` — the things a
    // declarator is written from. When what follows is `;`, `)`, `,`, `=`, `{` or `(`, the name is the
    // declarator and the allowance still decides, exactly as before.
    //
    // Nothing valid is taken from the expression reading by this: `Name Name Name` and `Name Name * Name` are not
    // expressions in any grammar, so the only statements that change are the ones that had no reading at all.
    if names_may_join.a_further_name && a_declarator_still_follows_the_name(p) {
        return true;
    }
    // A name the **caller's table** says is a type, joining a sequence that has already taken a name: that is the
    // shape a modifier macro leaves behind —
    //
    // ```text
    // MY_API Widget *p;         the ordinary spelling, which the shape rule above already reads
    // MY_API Widget const w;    …and the one it cannot: `const` is not a declarator, so the follower rule says
    //                           "no declarator follows" and the type would end at `const`
    // ```
    //
    // The first name is what makes this safe to ask: before a name has joined, a type name is what the *sequence*
    // is for, and `name_allowed` already decides it.
    if names_may_join.a_further_name
        && p.symbol_kind(p.current_token_text())
            .is_some_and(|kind| matches!(kind, SymbolKind::Type | SymbolKind::Template))
    {
        return true;
    }
    let complete = type_is_already_complete(p);
    let called = a_parenthesis_follows_the_name(p);
    if complete || called {
        return false;
    }
    name_allowed
}

/// Does what follows the name at the cursor still leave room for a **declarator**?
///
/// The question [`name_joins_the_type`] asks about a *second* name in the specifier sequence — the one an export
/// macro leaves behind. The name may be qualified (`EXPORT std::string g();`) or templated
/// (`MY_API Vector<int> *make();`), so the whole name is walked first and the token after it is what answers.
///
/// Walking *past* a template argument list rather than stopping at its `<` is the whole subtlety, and both
/// directions are real code:
///
/// ```text
/// MY_API Vector<int> *make();     a `*` after the list needs a declarator, so `Vector<int>` is the type
/// template MyType f<int>(int);    a `(` after the list calls the name, so `f<int>` is the *declarator's* name
/// ```
///
/// Answering at the `<` reads the second line as a type `MyType f<int>` with no declarator left, and the explicit
/// instantiation fails — which is exactly what a first version of this rule did.
pub(crate) fn a_declarator_still_follows_the_name(p: &CppParser) -> bool {
    let mut after = super::super::decls::next_significant_index(p, p.current_token_index());
    // **A directive between the name and its declarator is transparent to this question.** libstdc++ separates a
    // member's return type from its name with a conditional:
    //
    // ```cpp
    //       iterator                                   // bits/stl_vector.h, `erase`
    // #if __cplusplus >= 201103L
    //       erase(const_iterator __position)
    // ```
    //
    // Read without this, the follower of `iterator` was the `#`, which is neither an identifier nor a specifier, so
    // the name was refused the type position and became the **declarator**: the declaration then ended at the `#`,
    // and `bits/stl_vector.h` produced **no `erase` fact at all** (`facts_probe`: 0 facts for `erase`, 2 for
    // `push_back`). `members_of("std::vector")` therefore answered "no member `erase`" — the last false positive the
    // status probe found on a real project.
    //
    // The directive is skipped by its **logical line**, which is where `parse_preprocessor_directive` ends one: a
    // `\`-spliced directive is one line, and every token between here and there is the directive's own.
    while p.token_kind_at(after) == CppTokenKind::Hash {
        after = after_the_directive(p, after);
        after = super::super::decls::next_significant_index(p, after);
    }
    while p.token_kind_at(after) == CppTokenKind::Scope {
        let segment = super::super::decls::next_significant_index(p, after);
        if p.token_kind_at(segment) != CppTokenKind::Identifier {
            break;
        }
        after = super::super::decls::next_significant_index(p, segment);
    }

    // A template argument list is part of the name, so step over it. `angle_depth_delta` is the same rule the
    // lookahead scans use — including the `>>` that closes two lists at once.
    if p.token_kind_at(after) == CppTokenKind::Less {
        let mut depth = 0isize;
        let mut index = after;
        // The `<` at `after` is preceded by the name the walk above stepped over — see [`angle_depth_delta`].
        let mut previous = CppTokenKind::Identifier;
        while index < p.token_count() {
            let kind = p.token_kind_at(index);
            depth += angle_depth_delta(previous, kind);
            previous = kind;
            index += 1;
            if depth <= 0 {
                break;
            }
        }
        after = super::super::decls::next_significant_index(p, index.saturating_sub(1));
    }

    // An **identifier** after the name means the declaration has not reached its declarator yet — `unsigned
    // __int64 x`, `MY_API Widget *p`.
    matches!(
        p.token_kind_at(after),
        CppTokenKind::Identifier
            | CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            // **An operator-function-name is a declarator**, and it is the one that does not begin with an
            // identifier:
            //
            // ```cpp
            // #ifdef __glibcxx_string_view          // bits/basic_string.h:1025
            //   template<typename _Tp>
            //     _GLIBCXX20_CONSTEXPR
            //     _If_sv<_Tp, basic_string&>
            //     operator=(const _Tp& __svt)
            //     { return this->assign(__svt); }
            // #endif
            // ```
            //
            // Read without this, `_If_sv<_Tp, basic_string&>` was the *declarator's* name — the template-id
            // allowance below the caller makes for a variable template's partial specialization took it — so the
            // member came out as a **variable** called `_If_sv`, and the real declarator, its parameter list and
            // its body had nowhere to go: they landed beside the declarator instead of inside it, and every member
            // written after them was read as a child of that one declaration rather than as a member of the class.
            //
            // Nothing valid is taken from the expression reading by this: `Name operator` is not an expression in
            // any grammar, so the only statements that change are the ones that had no reading at all.
            | CppTokenKind::OperatorKeyword
    )
        // **A type keyword after the name**: the type has not started yet, so the name is still part of it.
        //
        // ```cpp
        // _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR    // bits/basic_string.h:1329
        // bool
        // empty() const _GLIBCXX_NOEXCEPT
        // { return _M_string_length == 0; }
        // ```
        //
        // Two unexpanded macros and then a keyword type. The first name joined the type (the sequence had nothing
        // yet), and the second was refused here because `bool` is not an identifier, a `*`, a `&` or an `operator`
        // — so `_GLIBCXX20_CONSTEXPR` became the *declarator's* name, the declaration came out as a **variable**
        // of that name, and `empty()`, its body and every member after it were read as children of it instead of
        // as members of the class. That is what `std::basic_string::empty` was missing for.
        //
        // A keyword type is what a declaration writes where a declarator cannot go, so it is evidence for the same
        // reason the `*` is: `Name bool` is not an expression in any grammar.
        || is_type_specifier_keyword(p.token_kind_at(after))
        // …and **a specifier keyword** for the same reason, one step further into the type:
        //
        // ```cpp
        // template<typename _CharT, typename _Traits, typename _Alloc>
        //   _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR      // bits/basic_string.h:3837
        //   inline basic_string<_CharT, _Traits, _Alloc>
        //   operator+(const basic_string<…>& __lhs, const basic_string<…>& __rhs)
        // ```
        //
        // Here the follower of the second macro is `inline`, and *its* follower question is asked about the
        // template-id, whose follower is the `operator`. Neither `inline` nor a keyword type can begin a
        // declarator, so each one says the same thing: the sequence has not reached the declarator yet, and the
        // name in front of it is still part of the type. Without this the file's first error sat on the
        // `operator+` — the whole tail of `bits/basic_string.h` unread, and the two `operator+` overloads, which
        // are how `std::string` is concatenated, were not members of anything.
        || is_a_declaration_specifier(p.token_kind_at(after))
}

/// May this keyword stand among a declaration's leading words — where a *declarator* can never go?
///
/// The companion of [`is_type_specifier_keyword`], and deliberately built from the lists that already exist
/// rather than from a third one: `storage_or_function_specifier` is the set the specifier sequence itself reads
/// as specifiers, and `const`/`volatile` are the two cv-qualifiers, which name no type but continue one.
///
/// It answers the same question for the *other* kind of specifier: a type keyword says the type is still coming,
/// and a specifier keyword says the same — `MY_API Widget const w;` and `MY_API inline Widget f();` are both
/// declarations whose type is not finished at the name.
pub(crate) fn is_a_declaration_specifier(kind: CppTokenKind) -> bool {
    storage_or_function_specifier(kind).is_some()
        || matches!(
            kind,
            CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
        )
}

/// Is the identifier at the cursor immediately followed by a `(`?
///
/// The signal that separates the type from the declarator in the one shape this parser used to need a type
/// table for:
///
/// ```text
/// Widget w(1, 2, 3);   `Widget` is the type, `w` the declarator — the `(` calls `w`
/// unsigned int x;      `x` is not called
/// void f();            `f` is the declarator already, by the keyword rule above
/// ```
///
/// A name followed by `(` in *type* position has no reading: nothing in a decl-specifier-seq is called, and a
/// type's own parentheses — `void (*)(int)` — belong to a declarator that has already been introduced by a
/// `*`. So the `(` can only mean the name is not part of the type, which is the reading the declaration needs
/// and the reason `Widget w(1, 2, 3);` no longer depends on the file having declared `Widget` first.
///
/// # What it is not allowed to break
///
/// The name is only refused when a *type* is already in hand. Asked about the first name of a declaration —
/// `g(1, 2);`, `A(B);` — the answer is no, because refusing there leaves the specifier sequence with nothing
/// and the declaration reading fails before the expression reading can be reached. Those statements have a
/// single name, so the name is taken for the type and the parentheses are left for the caller, which is
/// exactly what happened before this rule existed.
///
/// A name that legitimately joins a type and is *then* followed by a `(` — which is every function declarator
/// there is — never reaches the `(` for a different reason: [`type_is_already_complete`] has already answered
/// before this is asked. `void f(int)`, `Foo::Foo()` and `template <typename T> void g(T)` all stop at the
/// name for the older rule.
///
/// The name itself is exempt too, and that is the guard that keeps a *redeclaration* working:
/// `Widget Widget(1);` — a variable named like its type, or a function whose name is the type's — is left to
/// the type table rather than being split by this rule.
pub(crate) fn a_parenthesis_follows_the_name(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier {
        return false;
    }

    let Some(type_name) = p.declaration_type_name() else {
        // No type yet: this name can only *be* the type. See the note above.
        return false;
    };

    if p.token_text_at(p.current_token_index()) == type_name {
        return false;
    }

    // Over the name and any template argument list it carries, then look for the `(`. The scan stops at
    // anything that ends a name, so it cannot run into a `(` belonging to a later construct.
    //
    // A closing parenthesis ends the question in the other direction, and it has to be checked *before* the
    // operators below: `(a && b)` is a condition, and the `&&` in it would otherwise read as the reference
    // operator of a declarator. What separates the two is which comes first — `(a && b)` closes before the
    // operator is reached in a way that matters, while `void f(Args&&... a)` has the operator and no `)` in
    // between — so the scan answers on the first of the two it meets.
    //
    // A pointer, reference or pack operator is a stop as well, and it is the subtlest of the stops: in
    // `void g(Args&&... args)` the `(` after the operator belongs to the *declarator*, not to the name, and a
    // scan that ran past the `&&` would refuse `Args` its place in the type and leave the parameter without
    // one. Those operators are part of the type's own syntax, which is exactly what the name is being tested
    // for.
    let mut depth = 0isize;
    // The token before the one being looked at — see [`angle_depth_delta`]. The scan starts just past the name,
    // so the first token's predecessor is that very name, which *can* end a template-name: `<` right after it
    // opens a list, which is what `A<int>(x)` needs.
    let mut previous = CppTokenKind::Identifier;

    for kind in p.peek_token_kind_at(1..64) {
        match kind {
            CppTokenKind::LeftParen if depth <= 0 => return true,
            CppTokenKind::RightParen if depth <= 0 => return false,
            // The name ended without a `(`: an operator, a separator, an initialiser, a body, or the
            // `::` of a longer qualified name — which is the case `continues_a_qualified_name` owns.
            CppTokenKind::Identifier
            | CppTokenKind::Comma
            | CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftBracket
            | CppTokenKind::Assign
            | CppTokenKind::Colon
            | CppTokenKind::Scope
            | CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            | CppTokenKind::Ellipsis
            | CppTokenKind::Eof
            | CppTokenKind::None => return false,
            kind => depth += angle_depth_delta(previous, kind),
        }

        previous = kind;
    }

    false
}

/// Does the specifier just consumed already stand on its own as a complete type?
///
/// Asked by walking back to the previous significant token. A *qualifier* is not a type — `const`
/// alone is not one, so `const Point p` still has room for `Point` to join — while every keyword
/// that names a type, and every template-id's closing `>`, ends one.
///
/// # Why a class keyword is not one either
///
/// `struct`, `class`, `union` and `enum` name a *kind* of type and are not a type by themselves: what they
/// introduce still has to be named or given a body. Treating them as complete is what made
/// `struct Foo f;` declare a variable called `Foo` and then report `expected ';'` against `f` — the
/// elaborated-type-specifier spelling, which is how C code and a great deal of C++ still declares a variable of
/// a struct type. With them excluded, `Foo` joins the type as the elaborated name and `f` is the declarator.
///
/// The keyword is not left unrecognised: the branch above that parses a class-like head takes `struct Foo {`
/// and `struct Foo;` before this is ever asked, and `enum class E` is handled there too. What reaches here is
/// the bare keyword of a declaration whose name and declarator are still to come.
/// # What the walk steps over
/// # What the walk steps over
///
/// A run of `alignas(…)` specifiers, payloads and keywords alike: it is the one specifier whose tokens end in a
/// bracket, and neither its payload nor its keyword finishes a type. Stepping over the whole run is what keeps
/// `alignas(16) MyType value;` from reading `MyType` as a second name of a *finished* type.
///
/// Everything else that finishes no type is in the exclusion list at the end, and the walk is bounded by the
/// statement's own tokens — a `;`, a brace, or the start of the file.
///
/// # Why the indices are raw
///
/// [`CppParser::current_token_index`] is what the walk starts from, and it indexes the source text's token array.
/// [`CppParser::token_kind_at`] and [`CppParser::token_text_at`] index the same array, so every index here is raw
/// and every question is asked with the same accessor family. Mixing in an index counted over *significant* tokens
/// is the mistake this walk is most prone to, and it is invisible in the arithmetic.
pub(crate) fn type_is_already_complete(p: &CppParser) -> bool {
    let mut index = p.current_token_index();

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        // Trivia is not a token of the declaration.
        if matches!(
            kind,
            CppTokenKind::Whitespace
                | CppTokenKind::Newline
                | CppTokenKind::LineContinuation
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment
        ) {
            continue;
        }

        // A **cv-qualifier** neither finishes a type nor unfinishes one, and *where* it sits is what tells the two
        // spellings apart:
        //
        // ```text
        // const char w[]      the `const` comes *before* the type…
        // char const w[]      …and here it comes after, so the type was already finished when it arrived
        // ```
        //
        // Both are a qualified `char`, and in both the declarator is `w`. Judging the qualifier itself answered
        // "not complete" for the second spelling, so `w` joined the *type*: the declaration came out with no
        // declarator at all, and `char const w[2] = { 'a' };` was reported as a broken expression. (`char const* p`
        // was unaffected only because the `*` follows the qualifier and stops the sequence first.)
        //
        // So the walk steps over it and judges what is in front.
        if matches!(
            kind,
            CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
        ) {
            continue;
        }

        // A run of `alignas(…)` specifiers, payloads and keyword alike: it is the one specifier whose tokens end
        // in a bracket, and neither its payload nor its keyword finishes a type. Stepping over the whole run is
        // what keeps `alignas(16) MyType value;` from reading `MyType` as a second name of a *finished* type.
        if kind == CppTokenKind::AlignasKeyword {
            continue;
        }
        if kind == CppTokenKind::RightParen
            && let Some(keyword) = the_alignas_introducing_the_payload_ending_at(p, index)
        {
            if keyword == 0 {
                return false;
            }
            index = keyword;
            continue;
        }

        // A statement boundary means the walk ran out of *this* declaration without finding a type, which is the
        // opposite of finding a complete one.
        if matches!(
            kind,
            CppTokenKind::Semicolon
                | CppTokenKind::LeftBrace
                | CppTokenKind::RightBrace
                | CppTokenKind::Eof
                | CppTokenKind::None
        ) {
            return false;
        }

        // A class keyword names a *kind* of type and is not a type by itself — what it introduces still has to be
        // named or given a body. See the note above about `struct Foo f;`.
        return !matches!(
            kind,
            CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::EnumKeyword
        );
    }

    false
}

/// The raw index of the `alignas` keyword introducing the payload whose `)` is at `raw_index`, if that is what
/// the parenthesis is.
///
/// Walks back over the payload's own brackets to its `(` and answers only when the token before it is the
/// keyword. Bounded by the first token that cannot be inside an alignment — a `;`, a brace, or the start of the
/// file — so a parenthesis belonging to something else cannot make this walk run away.
pub(crate) fn the_alignas_introducing_the_payload_ending_at(
    p: &CppParser,
    raw_index: usize,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = raw_index;

    loop {
        match p.token_kind_at(cursor) {
            CppTokenKind::RightParen => depth += 1,
            CppTokenKind::LeftParen => {
                depth -= 1;
                if depth == 0 {
                    let keyword = the_significant_token_before(p, cursor)?;
                    return (p.token_kind_at(keyword) == CppTokenKind::AlignasKeyword)
                        .then_some(keyword);
                }
            }
            CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::Eof
            | CppTokenKind::None => return None,
            _ => {}
        }

        if cursor == 0 {
            return None;
        }
        cursor -= 1;
    }
}

/// The raw index of the nearest non-trivia token before `raw_index`, or `None` at the start of the file.
pub(crate) fn the_significant_token_before(p: &CppParser, raw_index: usize) -> Option<usize> {
    let mut cursor = raw_index;
    while cursor > 0 {
        cursor -= 1;
        if !matches!(
            p.token_kind_at(cursor),
            CppTokenKind::Whitespace
                | CppTokenKind::Newline
                | CppTokenKind::LineContinuation
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment
        ) {
            return Some(cursor);
        }
    }
    None
}

/// Is the cursor on `enum` followed by `class` or `struct`?
pub(crate) fn is_enum_class_head(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2).first(),
        Some(CppTokenKind::ClassKeyword) | Some(CppTokenKind::StructKeyword)
    )
}
