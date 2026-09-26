//! Grammar coverage: what the parser reads today, and what it does not.
//!
//! # Why a file like this exists
//!
//! A parser for C++ is never "done", and the useful question is never "does it work?" but "which constructs
//! does it read, and which ones does it give up on?". Without a file that answers it, the answer is folklore:
//! every gap is rediscovered by whoever hits it, and a construct that starts working is never noticed.
//!
//! So the two lists below are the answer. [`constructs_the_parser_reads`] pins the shapes that work, and
//! [`constructs_the_parser_does_not_read_yet`] pins the ones that do not — each with a comment saying what the
//! syntax is and why it is not covered.
//!
//! # What a "gap" means here
//!
//! A construct in the second list produces a syntax error or an `ErrorNode`. It does **not** mean the file
//! fails to parse: the parser is total, so the tokens are all still in the tree and everything around the gap
//! is read normally. That is the property the editor depends on, and it is why the gaps are worth listing
//! rather than hiding — a gap costs one construct, not the rest of the file.
//!
//! When one of these starts working, the test that pins it fails, and the fix is to move the line from the
//! second list to the first. That is the whole mechanism, and it is deliberately the cheapest one available.

use cpp_parser::{
    CppLexer, CppParser, CppSyntaxKind, CppSyntaxTree, CppTokenKind, Dialect, IncludedMacro,
    LexerConfig, MacroBody, MacroEnvironment, ParserConfig,
};

/// The three places a construct can be written, because the same tokens are read differently in each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Where {
    /// At file scope, outside every brace.
    File,
    /// Inside a function body.
    Body,
    /// As a member of a class body.
    Class,
}

/// Parse `fragment` in `place` and report whether it came out clean.
fn reads(fragment: &str, place: Where) -> Result<(), String> {
    let source = match place {
        Where::File => fragment.to_string(),
        Where::Body => format!("void probe() {{ {fragment} }}"),
        Where::Class => format!("struct Probe {{ {fragment} }};"),
    };

    let tree = CppParser::parse(&source, ParserConfig::default());
    report(&source, &tree)
}

/// Is the condition of the first `if`/`while`/`switch` in `fragment` a **declaration**?
///
/// The condition sits inside a `ParenExpr` that belongs to the statement, so this is one level in: the statement's
/// own child, and then whatever holds the condition. Both the `if (init; cond)` form (whose first part is a
/// declaration *and* an expression follows) and the `if (decl)` form land here, so the answer is about the
/// condition's shape and not about which form it is.
fn condition_is_a_declaration(fragment: &str) -> bool {
    let source = format!("void probe() {{ {fragment} }}");
    let tree = CppParser::parse(&source, ParserConfig::default());

    tree.get_red_root()
        .descendants()
        .filter(|node| {
            matches!(
                CppSyntaxKind::from(node.kind()),
                CppSyntaxKind::IfStat | CppSyntaxKind::WhileStat | CppSyntaxKind::SwitchStat
            )
        })
        .flat_map(|statement| statement.children())
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParenExpr)
        .flat_map(|paren| paren.children())
        .any(|held| CppSyntaxKind::from(held.kind()) == CppSyntaxKind::Declaration)
}

/// **A condition that declares a variable** — `if (Foo p = get())`, `while (const auto n = g())`.
///
/// The declaration in a condition is the one a **`for` header** reads: specifiers and declarators, ended by the
/// `)` that closes the condition rather than by a `;` of its own. Three readings came before it, and the first is
/// the one worth remembering:
///
/// ```text
/// if (Foo* p = get())        read as `Foo * p = get()` — a BinaryExpr, **no diagnostic at all**
/// if (Foo p = get())         `expected ), but get identifier` against the `=`
/// if (const auto n = g())    `expected primary expression` against the `=`
/// if (int n = g()) { }       the same, and the block after the condition became rubble
/// ```
///
/// The silent one is the reason this test asserts a **shape** and not just "it parses": every file in the census
/// was lossless and error-free on that reading, so no count could have moved. `docs/grammar-gaps.md` records it as
/// the second number of maintenance convention 29.
///
/// The negative half is what the fix has to keep: a condition that *names* something without declaring it stays an
/// expression, which is the case `parse_for_init_declaration`'s "did this name anything?" refusal exists for.
#[test]
fn a_condition_may_declare_a_variable() {
    assert_reads(
        Where::Body,
        &[
            "if (int n = g()) { }",
            "if (const size_t n = g()) { }",
            "if (Foo p = get()) { }",
            "if (Foo* p = get()) { }",
            "if (auto p = get()) { }",
            "if (const auto& r = f()) { }",
            "while (auto n = g()) { }",
            "switch (int n = g()) { }",
            // The two forms a condition has *besides* a declaration, which must keep reading as they did.
            "if (v.size()) { }",
            "if (i++) { }",
            "if (a && b) { }",
            "if (f(1, 2)) { }",
            // …and the C++17 initialiser form, whose declaration is a separate construct again.
            "if (int n = g(); n > 0) { }",
        ],
    );

    for source in [
        "if (int n = g()) { }",
        "if (Foo p = get()) { }",
        "if (Foo* p = get()) { }",
        "while (const auto n = g()) { }",
    ] {
        assert!(
            condition_is_a_declaration(source),
            "{source} must read its condition as a declaration"
        );
    }

    for source in ["if (v.size()) { }", "if (i++) { }", "if (a && b) { }"] {
        assert!(
            !condition_is_a_declaration(source),
            "{source} declares nothing, and the expression reading is the one it must keep"
        );
    }
}

/// **An allocation's parenthesised group is its initialiser**, and the predicate that used to read it as a
/// function type's parameter list.
///
/// `a_parameter_list_is_the_type` asks whether a `(` after a type is the type's own parameter list (`void (int)`)
/// or a group the caller owns. Its first version judged the group by its **first token**, so a name there decided
/// for the whole group:
///
/// ```text
/// new T(int, char)   a function type, spelled with its parameter list — both elements begin with a type
/// new T(a, *q)       an allocation of `T` initialised with `(a, *q)` — `*q` is not a parameter
/// ```
///
/// The second was read as the first, `*q` had no type, and the file reported `expected a type specifier` against
/// the `*` (measured in `bits/uses_allocator.h`, `bits/node_handle.h` and `memory_resource.h`). A parameter is a
/// type and then a declarator, so every element has to begin like a type — which is what the predicate now asks.
///
/// The negative half is the shape the predicate exists for: `void (int)` and `new (int(*)(int))()` are still
/// types, and a parameter list that really is one must stay one.
///
/// Two shapes of the same *family* are still not read, and they are **not** this rule's: both were already failing
/// when the predicate was fixed, which is why they are recorded as gaps rather than quietly left out.
#[test]
fn an_allocation_initialiser_is_not_a_parameter_list() {
    assert_reads(
        Where::Body,
        &[
            "auto p = new T(a, *q);",
            "auto p = new T(a, b);",
            "auto p = ::new (buf) T(a, *q);",
            "auto p = new (buf) T(a, *q, c);",
            "auto p = new T[4];",
            "auto p = new (int(*)(int))();",
            "void g(int, char);",
            "auto f = [](int, char) { };",
        ],
    );

    // The group is the **initialiser**: a `FunctionType` in the allocation would be the old misreading.
    assert_statement_kind(&[
        forbidding(
            shape("auto p = new T(a, *q);", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::FunctionType,
        ),
        forbidding(
            shape(
                "auto p = new (buf) T(a, *q);",
                Where::Body,
                CppSyntaxKind::Declaration,
            ),
            CppSyntaxKind::FunctionType,
        ),
    ]);

    assert_does_not_read_yet(
        Where::Body,
        &[
            (
                "auto p = new T(*q);",
                "an allocation whose only argument starts with `*`: `a_parenthesised_abstract_declarator_follows` \
                 claims the group first — a `*` right after a `(` is a parenthesised declarator, `void (*)(int)` — \
                 so `(*q)` is read as a declarator and the `q` inside it has nowhere to go",
            ),
            (
                "auto p = new (Widget)(1);",
                "an allocation of a **parenthesised** type used as an initialiser: the note on \
                 `parse_a_type_here` says this shape is why the placement/type split exists, and it reads \
                 cleanly as a *statement* (`new (Widget)(1);`) but not here. Found while writing this test",
            ),
        ],
    );
}

/// **A member head split by `#if`/`#else`, where the `#else` branch names the type.**
///
/// `bits/alloc_traits.h:430-438` writes one member's head in two branches — `requires … static constexpr void` and
/// `static __enable_if_t<…>` — and it is the shape behind that file's first error (line 454, reported on the
/// *next* member because the class body was already damaged).
///
/// # The reading, which is not the one this test first guessed
///
/// The gap was pinned with a diagnosis — "the function's own name is taken as a second word of the type" — and the
/// tree said something else once it was printed:
///
/// ```text
/// DeclSpecifierSeq   static  void  #else  static      <- both branches merged, and `void` named a type
///   InitDeclarator   Declarator(NameExpr `C`)          <- `C` became the **declarator**
/// PreprocessorDirective `#endif`
/// Declaration        TemplateType(`f`)  `(` `)` `{` `}`  <- the real member, as rubble
/// ```
///
/// `void` is in the *other* branch, and "a type has already been named" was true because of it — so the `#else`
/// branch's `__enable_if_t<…>` was refused the type position, became the declarator, and the declaration ended at
/// the `#endif` with no `;`. The fix is that a branch is an alternative: `#else`/`#elif` puts back the three things
/// the specifier loop started with (no type named *in this branch*, the one-name allowance unspent, and the evidence
/// window moved past the directive). `#endif` is deliberately not such a boundary — the tail after it needs what
/// the branches agreed on. `docs/grammar-gaps.md` B53 keeps the wrong first diagnosis next to the right one.
///
/// The negative half is the reading that must not change: a branch that names no type leaves the tail's name as the
/// type (`#else static` / `#endif` / `C f()`), and the second name of a split head without a macro is still the
/// declarator.
#[test]
fn a_split_member_head_may_name_its_type_in_the_else_branch() {
    assert_reads(
        Where::Class,
        &[
            // The shape the round fixed, shrunk, and then in the file's own spelling (constraint, trailing
            // `noexcept`, and a name that is a template-id).
            "template<typename _Tp>\n#if X\n  static void\n#else\n  static C\n#endif\n  f() { }",
            "template<typename _Tp, typename... _Args>\n#if X\n\trequires C<_Tp, _Args...>\n\tstatic constexpr void\n\
             #else\n\tstatic __enable_if_t<C<_Tp, _Args...>>\n#endif\n\tconstruct(_Tp* __p)\n\tnoexcept(g())\n\t{ }",
            // `#elif` opens the other branch just as `#else` does.
            "template<typename _Tp>\n#if X\n  static void\n#elif Y\n  static C\n#endif\n  f() { }",
            // A branch that names **no** type leaves the tail's name as the type: the reset must not cost this.
            "template<typename _Tp>\n#if X\n  static void\n#else\n  static\n#endif\n  C f() { }",
            // What already read, and must keep reading: a keyword in the `#else` branch, either branch naming its
            // own type, and no split at all.
            "template<typename _Tp>\n#if X\n  static void\n#else\n  static int\n#endif\n  f() { }",
            "template<typename _Tp>\n#if X\n  static C\n#else\n  static D\n#endif\n  f() { }",
            "template<typename _Tp>\n  static C\n  f() { }",
            "template<typename _Tp>\n  static C\n  f();",
            "template<typename _Tp>\n#if X\n  static C\n#else\n  static void\n#endif\n  f() { }",
            "template<typename _Tp>\n#if X\n  static void\n#else\n  static C\n#endif\n  int f() { }",
        ],
    );

    // The assertion the census cannot make: **which name is the declarator**. The wrong reading had `C` there and
    // `f() { }` as a declaration of its own; the right one has `f` there, the `#else` branch's name inside the type,
    // the `#endif` inside the type as well (it is one head written in two branches), and one member rather than two.
    let source = "struct Probe { template<typename _Tp>\n#if X\n  static void\n#else\n  static C\n#endif\n  f() { } };";
    let tree = CppParser::parse(source, ParserConfig::default());
    let declarator = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
        .expect("the member has a declarator");
    assert_eq!(
        declarator
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
            .map(|node| node.text().to_string())
            .as_deref(),
        Some("f"),
        "the member's own name is the declarator, not the name written in the `#else` branch"
    );
    assert_eq!(
        direct_members(source),
        1,
        "one head written in two branches is one member"
    );
    assert_eq!(
        declarator
            .parent()
            .and_then(|member| member
                .children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq))
            .map(|specifiers| specifiers.text().to_string())
            .unwrap_or_default()
            .matches('#')
            .count(),
        2,
        "both directives belong to the type's own node — the head is one sequence, not two"
    );

    // …and the one variant that is **not** this rule's, pinned rather than left to be rediscovered: the branch ends
    // with a *second* name (`MY_API C`), which needs the evidence "a name followed by a directive is not the
    // declarator" — refused on purpose, because a declaration's **initialiser** really may be written per branch
    // (`static const int n` / `#if X = 1; #else = 2; #endif`), and that shape reads today.
    assert_does_not_read_yet(
        Where::Class,
        &[(
            "template<typename _Tp>\n#if X\n  static MY_API C\n#else\n  static MY_API D\n#endif\n  f() { }",
            "a split head whose branches end with a **second** name: `C` is refused the type (the one-name allowance \
             was spent on `MY_API`) and becomes the declarator, so the declaration ends at the `#endif` without a \
             `;`. Reading it needs the directive itself as evidence, which the per-branch initialiser above forbids",
        )],
    );
}

/// The GNU spellings of `decltype` are that keyword, so a typedef of one is a declaration.
///
/// `typedef __typeof__(x) T;` was read as a *declarator* named `__typeof__` with a parameter list, and the
/// declaration then reported `expected ;` against the `T`. The lexer maps the three spellings to
/// `DecltypeKeyword`, which is the whole fix: everything that already knew what `decltype` is — the specifier
/// sequence, the declaration anchors, `a_decltype_here_is_a_type` — now answers for them too.
///
/// Measured: `bits/stl_heap.h`, `bits/stl_uninitialized.h` and `stddef.h` of the standard-library closure, which
/// are three of the files that went from failing to clean in the round this was written (`docs/std-library.md`).
#[test]
fn the_gnu_spellings_of_decltype_are_that_keyword() {
    assert_reads(
        Where::File,
        &[
            "typedef __typeof__(x) T;",
            "typedef __decltype(y) U;",
            "typedef __typeof(x) V;",
            "typedef decltype(z) W;",
            "__typeof__(x) v = 1;",
            "auto w = __typeof__(x)();",
        ],
    );

    // The four spellings are one kind of token, and the text is what still tells them apart.
    for spelling in ["decltype", "__typeof__", "__typeof", "__decltype"] {
        assert_eq!(
            CppLexer::new(spelling, LexerConfig::default(), &mut Vec::new())
                .tokenize()
                .first()
                .map(|token| token.kind),
            Some(CppTokenKind::DecltypeKeyword),
            "`{spelling}` must lex as the keyword it is"
        );
    }
}

/// **A class head written in more than one piece** — a macro before the name, and a directive before the base
/// clause.
///
/// Two shapes from the standard library's own heads, and both used to end the head too early, so the diagnostic
/// landed on a line that is not wrong:
///
/// ```text
/// class _GLIBCXX17_DEPRECATED unary_negate : public …   bits/stl_function.h:1021
///   a bare macro (no argument list) between the class-key and the name: the head was read as a class named
///   `_GLIBCXX17_DEPRECATED`, and `expected ;` landed on the `:` of the base clause
///
/// class move_iterator                                    bits/stl_iterator.h:1435
/// #ifdef __glibcxx_ranges
///   : public __detail::__move_iter_cat<_Iterator>
/// #endif
/// { … }
///   a directive between the name and the base clause: the head ended at the name, and `expected ;` landed on
///   the `:` of the branch
/// ```
///
/// The negative half is the reason both rules carry guards rather than being "read on": `struct S requires C<T>
/// { }` is not valid C++ and must still be reported (it is pinned in `concepts.rs`), and `class A final : B` is a
/// class named `A` — `final` is a spelling, not a token, so a second name spelled that way is never the macro.
#[test]
fn a_class_head_may_be_written_in_pieces() {
    assert_reads(
        Where::File,
        &[
            // A bare macro before the name, with each way a head can continue.
            "class MACRO Name : public Base { };",
            "class MACRO Name { };",
            "class MACRO Name;",
            "struct MACRO Name\n{\n};",
            "class MACRO Name\n#ifdef X\n  : public Base\n#endif\n{ };",
            // The parenthesised form that was already read (B46), unchanged.
            "typedef struct DECLSPEC_ALIGN (8) Header { int i; } Header;",
            // A directive between the name and the base clause, with and without a branch on the body.
            "class Name\n#ifdef X\n  : public Base\n#endif\n{ int member; };",
            "template<typename T>\nclass Name\n#if X\n  : public Base<T>\n#endif\n{ T value; };",
            // …and the shapes that must keep their reading.
            "class A final : public B { };",
            "class A final { };",
            "class MACRO final : public B { };",
        ],
    );

    // The class the head declares is the name, not the macro: a consumer asking for the class by name must find
    // it, which is the whole point of reading the macro out of the way.
    for (source, name) in [
        ("class MACRO Widget { };", "Widget"),
        ("class Widget\n#ifdef X\n: public B\n#endif\n{ };", "Widget"),
    ] {
        assert!(
            contains_a_class_named(source, name),
            "{source} must declare a class called `{name}`"
        );
    }
}

/// Is there a class-like definition named `name` in the fragment?
fn contains_a_class_named(source: &str, name: &str) -> bool {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();

    root.descendants().any(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef
        ) && node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| {
                token.kind() == cpp_parser::CppKind::Token(CppTokenKind::Identifier)
                    && token.text() == name
            })
    })
}

/// **A functional conversion with no arguments** — `int()`, `bool()`, `typename T::type()`.
///
/// The payload of a functional-notation conversion is an argument list, and an **empty** one is the commonest
/// there is: a default-constructed temporary. `parse_parenthesized_expression` refuses it — correctly, since it
/// reads an *expression* and `)` is not one — so both arms that read a functional conversion (a keyword type and
/// a `typename`-qualified one) had no payload and the whole expression failed:
///
/// ```text
/// return int();                                             expected primary expression against the `int`
/// return typename iterator_traits<_Iter>::iterator_category();   the same against the `typename`
/// ```
///
/// The second is `bits/stl_iterator_base_types.h:242`, which is what the round that fixed this measured.
///
/// The negative half is the reading that must not change: a *call* is not a conversion (`f()` is an
/// `IdentifierExpr`, not a `CastExpr`), and a bare `typename T::type` with no payload is still not an expression.
#[test]
fn a_functional_conversion_may_have_no_arguments() {
    assert_reads(
        Where::Body,
        &[
            "return int();",
            "return bool();",
            "return double();",
            "return typename A::type();",
            "return typename A<int>::type();",
            "return typename iterator_traits<_Iter>::iterator_category();",
            "return int(1);",
            "return typename A::type{};",
            "return typename A::type(1);",
            // …and the shapes that must keep their reading.
            "return f();",
            "return a.b();",
            "typename A::type();",
        ],
    );

    // A conversion is a cast; a call is a name.
    let kinds = |source: &str| {
        let tree = CppParser::parse(
            &format!("void probe() {{ {source} }}"),
            ParserConfig::default(),
        );
        let root = tree.get_red_root();

        (
            root.descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CastExpr),
            root.descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::IdentifierExpr),
        )
    };

    assert_eq!(kinds("return int();"), (true, false), "`int()` is a conversion");
    assert_eq!(
        kinds("return typename A::type();"),
        (true, false),
        "and so is a `typename`-qualified one"
    );
    assert_eq!(kinds("return f();"), (false, true), "`f()` is a call");
}

/// **A template argument that is an expression, and the comma that follows it.**
///
/// The list's argument reader tries a type first and falls back to an expression, and the fallback read a **full**
/// expression — the comma operator included. So the comma that separates two arguments was taken for the operator,
/// and the list came out one argument long:
///
/// ```text
/// S<3, 4> x;                       reads with **no diagnostic at all**: one argument `(3, 4)`
/// S<3, long> x;                    `expected primary expression` against `long` — the loud half
/// X<!C<T>, bool> f;                the argument ended at the comma with an empty right side, and the `bool`, the
///                                  `>`, the name and the body were rubble (bits/alloc_traits.h:530-534, which is
///                                  where the *next* first error of that file was after B53)
/// ```
///
/// The **type** reading is what hid it: `array<int, 3>` and `Grid<T, 3>::fill` put the comma after a type the type
/// reading has already accepted, so the fallback never ran for the spellings anyone would try by hand. The rule
/// belongs to the family [`Level`] lists in `exprs.rs` — a rule that spells its own separators reads one element —
/// and `docs/grammar-gaps.md` B54 records that the list said so before the code did.
///
/// The negative half is the other direction: a comma **inside parentheses** is still the comma operator, because
/// the parentheses are what say so, and a type argument is still read by the type reading.
#[test]
fn a_template_argument_read_as_an_expression_stops_at_the_comma() {
    assert_reads(
        Where::File,
        &[
            "S<3, 4> x;",
            "S<3, long> x;",
            "S<long, 3> x;",
            "S<N - 1, M> x;",
            "S<A::b, 2> x;",
            "S<f(1), 2> x;",
            "integer_sequence<int, 0, 1, 2> s;",
            "array<int, 3> a;",
            "Grid<T, 3>::fill x;",
            "template<typename T> using Not = X<!C<T>, int>;",
            "X<!C<T>, bool> f();",
            // A comma inside parentheses is the comma operator, and one argument holds it.
            "S<(a, b)> x;",
            "S<(a, b), 2> x;",
        ],
    );

    // The shape, because the silent half of the defect is a *count*: `S<3, 4>` was one argument holding a
    // comma-expression, and nothing about the parse said so.
    let arguments = |source: &str| {
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument)
            .count()
    };
    assert_eq!(arguments("S<3, 4> x;"), 2, "two arguments, not one");
    assert_eq!(arguments("S<3, long> x;"), 2, "and the same when the second is a type");
    assert_eq!(
        arguments("S<(a, b)> x;"),
        1,
        "parentheses make a comma an operator again"
    );

    // …and the second argument is really an argument: it has its own node rather than a comma-expression's right
    // operand.
    let tree = CppParser::parse("S<3, 4> x;", ParserConfig::default());
    assert!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument)
            .all(|argument| argument
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::LiteralExpr)),
        "each argument holds its own literal"
    );
}

/// **A qualifier the conditional decides** — `noexcept(…)` written once per branch after the parameter list.
///
/// The declarator's suffix reader has already run when the directive loop in `finish_init_declarator` reaches a
/// `#`, so a qualifier written *after* the conditional was still waiting when the loop came back. What came out was
/// well formed, lossless and **silent** — the shape of defect this file exists for:
///
/// ```text
/// void f()                    bits/alloc_traits.h:662
/// #if __cplusplus <= 201703L
///   noexcept(noexcept(__a.construct(__p, __args...)))
/// #else
///   noexcept(__is_nothrow_new_constructible<_Up, _Args...>)
/// #endif
///   { … }
/// ```
///
/// `noexcept` became the *next* declaration's type — a `BuiltinType` over `noexcept(true)`, two tokens that have
/// nothing to do with each other — the member after it became a `{ }`-initialised variable, and the body's own
/// directives landed at class scope. The diagnostic only appeared 27 lines later, on a member that was not at
/// fault. Asking for the qualifiers again after each directive is the fix; the assertion below is the one that
/// catches the silent half, because the *old* reading had no error at all in the one-branch spelling.
#[test]
fn a_conditional_may_decide_a_functions_qualifier() {
    assert_reads(
        Where::Class,
        &[
            "void f()\n#if A\n  noexcept(true)\n#else\n  noexcept(false)\n#endif\n  { }",
            "void f()\n#if A\n  noexcept(true)\n#else\n  noexcept(false)\n#endif\n  {\n#if A\n    g();\n#endif\n  }",
            "void f()\n#if A\n  const\n#else\n  const volatile\n#endif\n  { }",
            "template<typename _Up, typename... _Args>\n  void f(_Up* __p, _Args&&... __args)\n#if A\n\
             noexcept(noexcept(g(__p)))\n#else\n  noexcept(h<_Up>()) \n#endif\n  { }",
            "auto f()\n#if A\n  -> int\n#else\n  -> long\n#endif\n  { return 0; }",
            // The shapes that already read, and must keep reading: a head in two branches, a macro the
            // conditional decides, and a plain qualifier with no conditional at all.
            "#if defined(A)\nvoid g(void)\n#else\nvoid g()\n#endif\n{ }",
            "void f()\n#if A\n  _GLIBCXX_NOEXCEPT\n#endif\n  { }",
            "void f() noexcept(true) { }",
            "void f() const { }",
        ],
    );

    // **One member, and the body is a body.** The pre-fix reading of the single-branch spelling had no error, no
    // `ErrorNode` and no `MissingNode` — it was two declarations, with `{ }` as the second one's initialiser — so
    // `assert_reads` alone would have called it correct.
    let source = "struct Probe { void f()\n#if A\n  noexcept(true)\n#else\n  noexcept(false)\n#endif\n  { } };";
    let tree = CppParser::parse(source, ParserConfig::default());
    let body = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .expect("the `{ }` is the function's body");
    assert_eq!(
        body.parent().map(|parent| CppSyntaxKind::from(parent.kind())),
        Some(CppSyntaxKind::Declaration),
        "the body belongs to the declaration rather than being an initialiser"
    );
    assert_eq!(
        direct_members(source),
        1,
        "the qualifier and the body are one member, not two"
    );
    assert_eq!(
        body.parent()
            .map(|member| member
                .children()
                .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)
                .count()),
        Some(1),
        "`noexcept(true)` is not a type: the member has one specifier sequence, the one holding `void`"
    );
}

/// **A braced functional conversion as a template argument** — `X<int{}> m;`, and `int{}` as an expression.
///
/// C++11's `T{…}` is one construct in two spellings, and *both* halves of it were missing on the side that reads
/// **types**: the argument list, and the three lookahead scans that decide whether a `<` opens one at all.
///
/// ```text
/// auto a = int{};                  expected primary expression against the keyword — the conversion arm of
///                                  `parse_primary_expr` only took a `(` payload
/// g(int{});  return int{};         the same, in the two positions a value is passed
/// X<int{}> m;                      **read as `X < int{} > m`** — a comparison, no diagnostic, no ErrorNode
/// X<size_t{}> m;                   the same silent comparison: the type reading stopped at the `{`, and *that
///                                  stop was accepted as the end of the argument*
/// struct Q<T, int{}> { };          `Q` bare, arguments rubble — the scan stopped at the `{`
/// struct X<A, void_t<decltype(h(size_t{}))>> : B { };   the head read as body-less, base clause and body rubble
/// ```
///
/// Three of those are **silent** and one of them is the A0 shape this file exists for: the declaration the user
/// wrote is simply not in the tree, and every count stays the same. `bits/alloc_traits.h:941` is the fourth line,
/// which is where the round that fixed this measured it.
///
/// The fix is one idea in five places — *a matched brace pair belongs to what encloses it*: the functional
/// conversion arm takes a brace payload; the argument reader treats a trailing `{` as making the argument a value
/// rather than a type; and `a_matching_angle_bracket_follows`, `a_bare_template_id_is_here` and
/// `a_body_follows_the_class_head` each count braces the way they already counted `[` and `(`. The maintenance
/// convention about "the scans that count angles are three" is why they were all found in one pass rather than
/// one corpus probe each.
///
/// The negative half is the reason each scan stops at an *unmatched* brace: `if (a < b) { }`, `T x{a < b}` and
/// `struct S : B<C> { }` are not template-ids, and an unmatched `{`/`}` is still the enclosing declaration's own.
#[test]
fn a_braced_conversion_is_a_value_not_a_type() {
    assert_reads(
        Where::File,
        &[
            // The construct, in the positions it is written in.
            "X<int{}> m;",
            "X<size_t{}> m;",
            "X<int{}, long{}> m;",
            "X<A{1, 2}> m;",
            "X<Y<int{}>> m;",
            "using A3 = X<int{}>;",
            "using A4 = X<int{}, 1>;",
            "template<typename T> struct Q<T, int{}> { };",
            "template<typename _Alloc> struct __is_allocator<_Alloc, __void_t<typename _Alloc::value_type, \
             decltype(std::declval<_Alloc&>().allocate(size_t{}))>> : true_type { };",
            "auto a = int{};",
            "int f() { return int{}; }",
            "void h() { g(int{}); }",
            "void h() { bool b{true}; g(b); }",
            // What must keep its reading: comparisons, a braced initialiser that is not an argument, a class head
            // with a base clause, and the ordinary unbraced spellings.
            "void h() { if (a < b) { g(); } }",
            "void h() { bool r = a < b > c; }",
            "void h() { T x{a < b}; }",
            "void h() { auto l = [](int y) { return y < z; }; }",
            "struct S : B<C> { };",
            "template<typename T> struct Q<T, int> { };",
            "X<int> m;",
            "X<int, long> m;",
            "X<3, 4> m;",
            "void h() { int x{1}; }",
        ],
    );

    // **The silent half, which is the whole reason this test asserts shapes.** `X<int{}> m;` was a well-formed
    // `BinaryExpr` before the fix — `(X < int{}) > m` as an `ExpressionStat` — so `assert_reads` would have called
    // it correct, and a declaration read as a comparison is exactly the A0 defect `docs/grammar-gaps.md` opens
    // with.
    let tree = CppParser::parse("X<int{}> m;", ParserConfig::default());
    let root = tree.get_red_root();
    assert!(
        root.descendants()
            .all(|node| CppSyntaxKind::from(node.kind()) != CppSyntaxKind::ExpressionStat),
        "a declaration is not an expression statement"
    );
    let declarator = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
        .expect("the declaration has a declarator");
    assert_eq!(
        declarator
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
            .map(|node| node.text().to_string())
            .as_deref(),
        Some("m"),
        "the declarator is the name written after the type"
    );
    assert_eq!(
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument)
            .count(),
        1,
        "the braces are inside **one** argument of the type"
    );

    // The class head: the arguments belong to the name, and there is a body.
    let source = "template<typename T> struct Q<T, int{}> : B { };";
    let tree = CppParser::parse(source, ParserConfig::default());
    assert!(
        contains(source, CppSyntaxKind::ClassBody),
        "the head has a body, so the `{{` was not taken for a braced-init-list"
    );
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument)
            .count(),
        2,
        "and the name's argument list holds both arguments"
    );

    // The expression: `int{}` is a conversion, `int(x)` is still a declaration, and a plain `int{};` statement is
    // the conversion rather than a declaration of nothing.
    assert!(
        contains("void h() { auto a = int{}; }", CppSyntaxKind::CastExpr),
        "`int{{}}` is a functional conversion like `int()`"
    );
    assert!(
        contains("void h() { int(x); }", CppSyntaxKind::Declaration),
        "`int(x);` stays the declaration it is — the parenthesis spelling is a declarator's"
    );
}

/// **A directive between the requirements of a requires-expression** — the last of the seams.
///
/// The body is "one requirement per `;`", and a `#` at the position where a requirement begins had no reading at
/// all, so the whole requires-expression failed. libstdc++ writes the two alternatives of one requirement in two
/// branches, and `bits/alloc_traits.h:140` is where this was found:
///
/// ```cpp
/// template<typename _Tp, typename... _Args>
///   static constexpr bool __can_construct_at
///     = requires (_Tp* __p, _Args&&... __args) {
/// #if __cpp_constexpr_dynamic_alloc
///         std::construct_at(__p, std::forward<_Args>(__args)...);
/// #else
///         ::new((void*)__p) _Tp(std::forward<_Args>(__args)...);
/// #endif
///       };
/// ```
///
/// **What it cost, and why this is the last of a chain**: the failed member also closed the *class* early (the
/// known gap pinned at the bottom of this test), so every member after it was read at namespace scope and the
/// file's only diagnostic landed on the leftover `}` at its very end — line 1053 of 1053. Moving that error back
/// one construct at a time is what the five fixes before this one did (`docs/roadmap.md` §2.0: 454 → 536 → 689 →
/// 941 → 1053 → clean).
///
/// The shape assertion is the real one: a directive is a node of the requires-expression, the requirements are
/// still requirements, and the member stays a member.
#[test]
fn a_conditional_may_decide_a_requirement() {
    assert_reads(
        Where::Class,
        &[
            "static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#endif\n  };",
            "static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#else\n  t.g();\n#endif\n  };",
            "static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#endif\n#if Y\n  t.h();\n#endif\n  };",
            "static constexpr bool ok = requires (T t) {\n#if X\n  typename T::value_type;\n#else\n  { t.f() } noexcept -> int;\n#endif\n  };",
            "static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#endif\n  t.g();\n  };",
            "template<typename T>\n  static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#else\n  t.g();\n#endif\n  };",
            // What must keep reading: the same requires-expression without a conditional, and a directive that
            // decides the *whole* member rather than a requirement.
            "static constexpr bool ok = requires (T t) { t.f(); };",
            "static constexpr bool ok = requires (T t) { typename T::value_type; { t.f() } noexcept -> int; };",
        ],
    );

    // The whole member written once per branch — asserted as a **whole file**, because `Where::Class` puts the
    // closing brace on the same line as the fragment's last line, and `#endif }` is not something any compiler
    // accepts (see the note below).
    assert!(
        reads(
            "struct Probe {\n#if X\n  static constexpr bool ok = requires (T t) { t.f(); };\n#else\n  \
             static constexpr bool ok = requires (T t) { t.g(); };\n#endif\n};",
            Where::File,
        )
        .is_ok(),
        "a member written once per branch reads, with each directive on a line of its own"
    );

    // **One spelling is deliberately absent from that list**: the whole member written per branch with the
    // `#endif` **sharing its line** with the class's closing brace —
    //
    // ```cpp
    // struct S {
    // #if X
    //   static constexpr bool ok = requires (T t) { t.f(); };
    // #else
    //   static constexpr bool ok = requires (T t) { t.g(); };
    // #endif };            // ← `};` are extra tokens on the directive's line
    // ```
    //
    // It does not parse here, and it does not compile either: a preprocessing directive runs to the end of its
    // line, so `};` are extra tokens after `#endif` — g++ says `warning: extra tokens at end of '#endif'
    // directive` and then `error: expected '}' at end of input`, which is the same complaint this parser makes.
    // Pinned as a comment rather than as a test case because there is nothing to fix: convention 36 is "verify
    // the fragment with the compiler", and this fragment is wrong.

    // **The shape.** A directive inside the body is a child of the requires-expression, and the requirements on
    // both sides of it are still requirements — the failure mode this guards against is the body being abandoned
    // and the tokens becoming rubble, which reads as "fine" to every count in the census.
    let source = "struct Probe { static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#else\n  t.g();\n#endif\n  }; };";
    let tree = CppParser::parse(source, ParserConfig::default());
    let requires = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::RequiresExpr)
        .expect("the member holds a requires-expression");
    let inside = |kind| {
        requires
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
            .count()
    };
    assert_eq!(inside(CppSyntaxKind::Requirement), 2, "one requirement per branch");
    assert_eq!(
        inside(CppSyntaxKind::PreprocessorDirective),
        3,
        "the `#if`, `#else` and `#endif` are the expression's own children"
    );
    assert_eq!(
        direct_members("struct Probe { static constexpr bool ok = requires (T t) {\n#if X\n  t.f();\n#else\n  t.g();\n#endif\n  };\n  int after;\n};"),
        2,
        "the member with the conditional in it is a member, and so is the one after it"
    );
}

/// **A member that fails with an unbalanced brace keeps the class and its members** — the other half of the
/// recovery contract.
///
/// [`a_member_the_parser_gives_up_on_keeps_the_members_after_it_members`] covers the member that fails on a
/// *missing* `;`, and closing its markers with their end events was the fix. This is the half that fix could not
/// reach, because it is about **tokens** rather than about nodes: when the abandoned member had consumed a `{`
/// that never met its `}`, the class body spent its own `}` on it, ended early, and every member after it was read
/// at **file scope** — which is how `bits/alloc_traits.h` came to report its only diagnostic 900 lines after the
/// member that was actually broken (`docs/grammar-gaps.md` B58).
///
/// The fix is a **brace debt**: a failed member is charged for the braces it consumed and never closed, and the
/// body pays that debt by reading the next `}`s as error nodes before it is allowed to end. Both paths a failure
/// can take are charged — the attempt that stops past the member's first token (the events still hold it) and the
/// attempt that rolls back, whose tokens the loop then re-reads one at a time as rubble.
///
/// What is asserted is the question the defect is about — *is the member after the broken one still a member* —
/// rather than the shape of the rubble, for the same reason the sibling test gives: how many error nodes a given
/// piece of rubbish leaves is not a promise worth making.
#[test]
fn a_failed_member_with_an_unbalanced_brace_keeps_the_members_after_it_members() {
    for (source, note) in [
        (
            // A requirement split **mid-expression** by a directive: the `;` is in the other branch, which is
            // invalid code — and the input that leaves the `{` of the requires body unmatched when the member
            // fails. This is the reproduction B58 was written from.
            "struct Probe {\n  static constexpr bool ok = requires (T t) {\n#if X\n    t.f()\n#endif\n    ;\n  };\n  int after;\n};",
            "a requires-expression whose requirement is split by a directive",
        ),
        (
            // The same shape one level down: a function body left open by the failed member.
            "struct Probe {\n  void f() {\n#if X\n    g()\n#endif\n    ;\n  };\n  int after;\n};",
            "a function body whose statement is split by a directive",
        ),
        (
            // …and the file-scope version of the same input, which used to cost the whole class.
            "struct Probe {\n  int x = 1\n  int after;\n};",
            "a member with no `;` at all — the case the sibling test already pins, kept here so the two halves \
             are checked by one loop",
        ),
    ] {
        assert!(
            has_a_member_containing(source, "after"),
            "the member after a broken one is still a member: {note}\n{source}"
        );
    }
}

/// **A `typedef` may carry an attribute after its declarator** — the shape GCC's intrinsic headers are built from.
///
/// The ordinary declaration path has read attributes in this position since the round that added them
/// (`int x [[maybe_unused]] = 1;`), and the `typedef` rule is a *second* path — it spells its own declarator loop
/// and never reaches `finish_init_declarator` — so it was missing them. Eight files of the closure had this as
/// their first error, and it is the construct their whole type zoo is made of:
///
/// ```c
/// typedef int __v4si_u __attribute__ ((__vector_size__ (16), __may_alias__, __aligned__ (1)));
/// typedef short __v32hi __attribute__ ((__vector_size__ (64)));           // avx512bwintrin.h:361
/// typedef double __v8df __attribute__ ((__vector_size__ (64)));           // avx512fintrin.h:3817
/// typedef int v4 [[deprecated]];                                          // the standard spelling, same position
/// ```
///
/// Both spellings go through one rule — `parse_attribute_specifiers` reads `[[…]]`, `__attribute__((…))` and
/// `__declspec(…)` into the same node — so the fix is one call rather than one per compiler. The negative half
/// keeps the two positions apart: an attribute *before* the type is a specifier and was already read, and a
/// `typedef` with a comma-separated list must still declare every name.
#[test]
fn a_typedef_may_carry_an_attribute() {
    assert_reads(
        Where::File,
        &[
            "typedef int v4 __attribute__ ((__vector_size__ (16)));",
            "typedef int v4 [[deprecated]];",
            "typedef int v4 __attribute__ ((__vector_size__ (8), __may_alias__));",
            "typedef int __m64 __attribute__ ((__vector_size__ (8), __may_alias__));",
            // The spliced spelling the headers actually use, `\` and all.
            "typedef int __v4si_u __attribute__ ((__vector_size__ (16),\t\\\n\t\t\t\t     __may_alias__, __aligned__ (1)));",
            // What must keep reading: the attribute in front of the type, a list of declarators, and a typedef
            // with no attribute at all.
            "typedef int __attribute__((aligned(8))) v4;",
            "typedef WCHAR *PWCHAR, *LPWCH;",
            "typedef int Integer;",
            "int x [[maybe_unused]] = 1;",
            "void f(void) __attribute__((noreturn));",
        ],
    );

    // The shape: the attribute is the declaration's own node, and the name the typedef introduces is usable as a
    // type afterwards — which is the property the declaration rule exists for.
    let source = "typedef int v4 [[deprecated]];";
    let tree = CppParser::parse(source, ParserConfig::default());
    let typedef = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TypedefDecl)
        .expect("the declaration is a typedef");
    assert_eq!(
        typedef
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList)
            .count(),
        1,
        "the attribute belongs to the typedef rather than being rubble"
    );
    assert!(
        contains(
            "typedef int v4 [[deprecated]]; v4 x;",
            CppSyntaxKind::Declaration
        ),
        "and the name it declares is a type name afterwards"
    );
}

/// **A conditional may decide an attribute** — the position between a template head and the declaration it wraps.
///
/// That position already had a rule: an attribute there belongs to neither the head nor the specifier sequence, so
/// the declaration rule reads it itself (`template <typename T> [[nodiscard]] T p();`). What it did *not* have was
/// the **directive**, and a conditional in front of the first specifier is not the specifier sequence's to read —
/// it reads directives only *between* specifiers, because a declaration that begins with one is the caller's
/// (`docs/grammar-gaps.md` B60):
///
/// ```cpp
/// template<typename _Ex>                                  // bits/nested_exception.h:203
/// # if ! __cpp_rtti
///   [[__gnu__::__always_inline__]]
/// #endif
///   inline void
///   rethrow_if_nested(const _Ex& __ex)
/// ```
///
/// The `.tcc` files and libstdc++'s inline definitions write the same two orders, and with the single attribute
/// pass the whole declaration failed — the file came out as bare tokens with the diagnostic on the attribute. The
/// fix alternates the two, the same shape as the directive/macro alternation in `finish_init_declarator`.
///
/// The negative half is what must not change: an attribute at the start of an ordinary declaration is a
/// *specifier* (the sequence owns it), and a directive with no template head around it is read by the caller that
/// asked for the declaration.
#[test]
fn a_conditional_may_decide_an_attribute() {
    assert_reads(
        Where::File,
        &[
            "template<typename T>\n#if X\n[[attr]]\n#endif\ninline void f() { }",
            "template<typename T>\n[[attr]]\n#if X\n#endif\ninline void f() { }",
            "template<typename _Ex>\n# if ! __cpp_rtti\n  [[__gnu__::__always_inline__]]\n#endif\n  \
             inline void\n  rethrow_if_nested(const _Ex& __ex)\n  { }",
            "template<typename T>\n[[a]]\n#if X\n[[b]]\n#endif\nvoid f() { }",
            // What must keep reading: the attribute with no conditional, the conditional with no attribute, and
            // an attribute in front of an ordinary declaration.
            "template<typename T>\n[[nodiscard]] T p();",
            "template<typename T>\n#if X\n#endif\ninline void f() { }",
            "[[nodiscard]] int x;",
            "int x [[maybe_unused]];",
        ],
    );

    // The shape: the attribute and both directives belong to the template declaration, and the declaration is a
    // declaration — not the bare tokens the failing reading left behind.
    let source = "template<typename _Ex>\n# if ! __cpp_rtti\n  [[__gnu__::__always_inline__]]\n#endif\n  \
                  inline void\n  rethrow_if_nested(const _Ex& __ex)\n  { }";
    let tree = CppParser::parse(source, ParserConfig::default());
    let declaration = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration)
        .expect("the head and the declaration are one declaration");
    assert_eq!(
        declaration
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList)
            .count(),
        1,
        "the attribute is inside the declaration it belongs to"
    );
    assert!(
        declaration
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateDecl),
        "and so is the template head"
    );
}

/// **A type the target compiler spells** — `__int128`, `_Float16`, `__int64` — and the dialect that decides it.
///
/// A handful of reserved spellings have no single answer: `__int128` is a builtin type to GCC and Clang and a
/// plain name to cl.exe, `__int64` is the other way round, and `_Float16` belongs to GCC. Read as a *name* — the
/// only reading available before the dialect existed — the failure is silent and shaped like a declaration of
/// something else (`docs/grammar-gaps.md` B61, measured on `bits/bmi2intrin.h`):
///
/// ```text
/// unsigned __int128 x;      Declaration[ DeclSpecifierSeq[unsigned]  InitDeclarator[__int128]  MacroCall[x] ]
///                           — a variable named `__int128` whose *suffix* is the macro `x`
/// _Float16 h = 1;           the declaration fails outright: the name `h` is taken into the type
/// ```
///
/// The fix is a [`Dialect`] on the parser: the spellings are read as `BuiltinType` specifiers when the target
/// spells them that way, and stay names when it does not. The shape assertion is the one that matters — the
/// wrong reading had **no diagnostic at all**, so `assert_reads` called it correct.
///
/// The negative half is the reason this is a dialect and not a longer spelling list: under MSVC `__int128` must
/// stay a name, because that is exactly how MinGW's `_mingw.h` uses it (`typedef int __int128 __attribute__
/// ((__mode__ (TI)));`, in the branch a compiler without `__int128` takes), and `__int64` must stay a name under
/// GNU, because MinGW's `#define __int64 long long` is what makes it a type there.
#[test]
fn a_type_may_be_spelled_by_the_compiler() {
    // GNU first: the parser's own default, and the dialect its corpus is compiled by.
    assert_reads(
        Where::File,
        &[
            "unsigned __int128 x;",
            "__int128 x;",
            "void f() { auto r = (unsigned __int128) 1; }",
            "using T = unsigned __int128;",
            "typedef unsigned __int128 u128;",
            "struct S { unsigned __int128 big; };",
            "void f(unsigned __int128 v);",
            "void f() { _Float16 h = 1; }",
            "void f() { __bf16 b = 1; }",
            "void f() { __float128 q = 1; }",
            // …and the reserved names that are *not* types stay what they were.
            "void f(int *__restrict p);",
            "__extension__ inline int g();",
        ],
    );

    // **The shape**: a `BuiltinType` specifier whose text is the spelling, and a declarator that is the name
    // written after it — not a variable named `__int128` with a macro suffix.
    let tree = CppParser::parse("unsigned __int128 x;", ParserConfig::default());
    let root = tree.get_red_root();
    let builtin = root
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BuiltinType)
        .map(|node| node.text().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        builtin,
        vec!["unsigned ".to_string(), "__int128 ".to_string()],
        "both words are type specifiers"
    );
    assert!(
        root.descendants()
            .all(|node| CppSyntaxKind::from(node.kind()) != CppSyntaxKind::MacroCall),
        "and the declarator is not read as a macro suffix"
    );
    assert_eq!(
        root.descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
            .and_then(|declarator| declarator
                .descendants()
                .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr))
            .map(|name| name.text().to_string())
            .as_deref(),
        Some("x"),
        "the declared name is the one written last"
    );

    // **MSVC**: `__int64` and friends are types there, and `__int128` is not — which is how a header that
    // typedefs it reads.
    let msvc = |source: &str| {
        CppParser::parse(
            source,
            ParserConfig::default().with_dialect(Dialect::Msvc),
        )
    };
    assert!(
        msvc("__int64 big;").get_errors().is_empty(),
        "`__int64` is a type to MSVC"
    );
    assert!(
        msvc("typedef int __int128 __attribute__ ((__mode__ (TI)));")
            .get_errors()
            .is_empty(),
        "and `__int128` is a name there, which is what MinGW's typedef branch needs"
    );
    assert!(
        !msvc("struct S { __int64 big; };").get_errors().is_empty()
            || msvc("struct S { __int64 big; };")
                .get_red_root()
                .descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BuiltinType)
                .any(|node| node.text().to_string().trim() == "__int64"),
        "`__int64` is read as a type to MSVC, not as a variable's name"
    );
    // …and the same source under GNU keeps the other reading: `__int64` is a *name* there, because MinGW's
    // `#define __int64 long long` is what makes it a type in that configuration.
    assert!(
        CppParser::parse("typedef int __int64;", ParserConfig::default())
            .get_errors()
            .is_empty(),
        "under GNU `__int64` is an ordinary name, so a typedef of it declares one"
    );
}

/// **A braced body settles whether a parenthesised group was a parameter list.**
///
/// `T f(U)` is two readings at once — a function taking an unnamed parameter of type `U`, or a variable `f`
/// direct-initialised with the expression `U` — and the standard keeps both. What no reading survives is a `{`
/// after it: a declarator takes **one** initializer, so a body means the group was the parameter list. Without
/// that evidence the preference for the initializer reading (which exists for `Max(a, b);`, a declaration with no
/// type at all) took these definitions apart, and the diagnostic landed on the *body*:
///
/// ```cpp
/// _GLIBCXX20_CONSTEXPR
/// inline _Iter_less_val
/// __iter_comp_val(_Iter_less_iter)          // bits/predefined_ops.h:79
/// { return _Iter_less_val(); }              // a declarator takes only one initializer
///
/// template<typename _Ex>
///   __attribute__ ((__always_inline__))
///   inline exception_ptr
///   make_exception_ptr(_Ex) _GLIBCXX_USE_NOEXCEPT    // bits/exception_ptr.h:283 — a macro suffix in between
///   { return exception_ptr(); }
/// ```
///
/// Five files of the closure had this as their first error (`exception_ptr.h`, `predefined_ops.h`, `cmath`,
/// `helper_functions.h`, `type_traits.h`).
///
/// The evidence is asked with the **real readers** — parse the group as a parameter list, let the suffix reader
/// run (macro suffix, `noexcept`, trailing return type), keep it only if a `{` is what it reaches — rather than
/// with a scan that would have to know their vocabulary. And only where a definition is legal: **not inside a
/// body**, where `T x(y) { }` is a declaration followed by a block.
///
/// The negative half is that second fact plus the case the preference exists for: the initializer reading must
/// keep `T x(y);`, `Max(a, b);` and a call.
#[test]
fn a_body_settles_whether_the_group_was_a_parameter_list() {
    assert_reads(
        Where::File,
        &[
            "T f(U) { return X(); }",
            "inline T f(U) { return X(); }",
            "T f(U, V) { return X(); }",
            "T f(U) noexcept { return X(); }",
            "auto f(U) -> T { return X(); }",
            "MACRO\ninline T\nf(U)\n{ return X(); }",
            "_GLIBCXX20_CONSTEXPR\ninline _Iter_less_val\n__iter_comp_val(_Iter_less_iter)\n\
             { return _Iter_less_val(); }",
            "template<typename _Ex>\n  __attribute__ ((__always_inline__))\n  inline exception_ptr\n  \
             make_exception_ptr(_Ex) _GLIBCXX_USE_NOEXCEPT\n  { return exception_ptr(); }",
            // What must keep its reading, in the place where the *other* reading is the valid one.
            "T x(y);",
            "Max(a, b);",
            "T f(U);",
            "void g() { T x(y); }",
            "void g() { T x(y); { h(); } }",
            "void g() { h(x); }",
            "struct S { T f(U) { return X(); } };",
        ],
    );

    // The shape, both ways round: a definition has a **parameter list** and a body, a variable has an
    // **initializer** and no parameter list.
    let parts = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        let root = tree.get_red_root();
        let has = |kind| {
            root.descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == kind)
        };
        (
            has(CppSyntaxKind::ParameterList),
            has(CppSyntaxKind::Initializer),
            has(CppSyntaxKind::CompoundStat),
            root.descendants()
                .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
                .and_then(|declarator| {
                    declarator
                        .descendants()
                        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
                })
                .map(|name| name.text().to_string())
                .unwrap_or_default(),
        )
    };

    assert_eq!(
        parts("T f(U) { return X(); }"),
        (true, false, true, "f".to_string()),
        "a group followed by a body is a parameter list, and the declarator is the function's name"
    );
    assert_eq!(
        parts("T x(y);"),
        (false, true, false, "x".to_string()),
        "and a group that ends the declaration is an initializer"
    );
}

/// **A cast of a cast is still a cast** — `(T)(U) 1`, and the run of groups that makes it look like a call.
///
/// `(T)…` is read as a cast only on evidence, because `(f)(x)` is a *call* and the callee must not be lost. The
/// evidence the cast branch uses is "an operand follows the `)`" (two operands in a row is not an expression in
/// any grammar) — and the `(` that begins a continuation was deliberately left out of that set, which made a
/// **run of groups** unreadable: the token after the first `)` is another `(`, so the question was never asked.
/// GCC's own intrinsic headers write exactly that, four of them:
///
/// ```cpp
/// return (__m512bh) __builtin_ia32_minmaxbf16512_mask ((__v32bf) __A,
///                                                      (__v32bf)(__m512bh)     // avx10_2-512minmaxintrin.h:40
///                                                      _mm512_setzero_si512 (),
///                                                      (__mmask32) -1);
/// ```
///
/// So the scan steps over a **run** of balanced groups and asks its question about the token after the last of
/// them. That the token being stepped over is the ambiguous one is what makes the scan safe: `(f)(a)` (nothing
/// after) and `(f)(a)(b)` (a `;` after the second group) keep their readings, and the shapes below pin that.
#[test]
fn a_cast_of_a_cast_is_still_a_cast() {
    assert_reads(
        Where::Body,
        &[
            "(T)(U) 1;",
            "(T)(U) g();",
            "(T)(U)(V) 1;",
            "auto r = (T)(U) 1;",
            "auto r = (T)(U)(V) g ();",
            "h((T)(U) g(), 1);",
            "return (A) __builtin ((B) x, (C)(D)\n  g (), (E) -1);",
            "(int)(char) 1;",
            "(int)(char)(long) 1;",
            // What must keep its reading: a call, a chain of calls, and the operators that make `(` ambiguous.
            "(f)(a);",
            "(f)(a)(b);",
            "(f)(a) + 1;",
            "(a)[b];",
            "(a) - b;",
            "(a) *b;",
        ],
    );

    // **The shape**, because both readings parse: a cast of a cast is two `CastExpr`s, a call through
    // parentheses is a `CallExpr` over a `ParenExpr`, and a chain is two `CallExpr`s.
    let counts = |source: &str| {
        let tree = CppParser::parse(&format!("void probe() {{ {source} }}"), ParserConfig::default());
        let root = tree.get_red_root();
        let count = |kind| {
            root.descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
                .count()
        };
        (count(CppSyntaxKind::CastExpr), count(CppSyntaxKind::CallExpr))
    };

    assert_eq!(counts("(T)(U) 1;"), (2, 0), "two casts, no call");
    assert_eq!(counts("(T)(U)(V) 1;"), (3, 0), "…and the run can be longer");
    assert_eq!(counts("(f)(a);"), (0, 1), "a call through parentheses stays a call");
    assert_eq!(counts("(f)(a)(b);"), (0, 2), "and a chain of calls stays a chain");
    assert_eq!(
        counts("(f)(a) + 1;"),
        (0, 1),
        "`+` is ambiguous, so the call reading is the one kept"
    );
}

/// **A conditional inside a template head, or where a name segment stands** — three seams of the same family.
///
/// The directive seams were done for statements, declarations, class members, initialisers and requirements
/// (`docs/grammar-gaps.md` §2.1 and B57/B60); these are the three that were left, and each is a real spelling in
/// libstdc++:
///
/// ```cpp
/// template<typename _Tp, bool _TreatAsBytes =           // bits/cpp_type_traits.h:620 — the default argument
/// #if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
///       __is_integer<_Tp>::__value
/// #else
///       __is_byte<_Tp>::__value
/// #endif
///         >
///
///     vector<_Tp, _Alloc>::                             // bits/vector.tcc:133 — a name segment
/// #if __cplusplus >= 201103L
///     insert(const_iterator __position, const value_type& __x)
/// #else
///     insert(iterator __position, const value_type& __x)
/// #endif
///     { … }
///
/// X<                                                   // the argument itself
/// #if A
///   1
/// #else
///   2
/// #endif
///   > x;
/// ```
///
/// Two of the three needed more than "read the directive and go round": the default argument is written **once
/// per branch**, so an `#else` there spells the same parameter's value again rather than starting a new
/// parameter (the value reading is factored into `parse_a_default_value` for exactly that), and the `#endif`
/// after it ends the *parameter*, so the list must re-ask for its `,`/`>` rather than for another parameter.
///
/// Still **not** read, and pinned below: a declaration written once per branch where each branch supplies its own
/// tail *and its own `;`* (`bits/stl_iterator.h:3090`).
#[test]
fn a_conditional_may_decide_a_template_head() {
    assert_reads(
        Where::File,
        &[
            // The default argument of a template parameter, one branch and two.
            "template<int N =\n#if X\n  1\n#endif\n  > void f();",
            "template<int N =\n#if X\n  1\n#else\n  2\n#endif\n  > void f();",
            "template<typename T, bool B =\n#if X\n  A<T>::value\n#else\n  B<T>::value\n#endif\n  > struct C { };",
            // …and the same with the directive *between* parameters.
            "template<typename T,\n#if X\n  typename U\n#else\n  typename U\n#endif\n  > void f();",
            // A template argument written per branch.
            "X<\n#if A\n  1\n#else\n  2\n#endif\n  > x;",
            // A name segment, which in the real file is a member definition whose head is written per branch.
            "void S::\n#if X\nf()\n#else\ng()\n#endif\n{ }",
            "template<typename T>\ntypename V<T>::iterator\nV<T>::\n#if X\ninsert(int x)\n#else\ninsert(long x)\n#endif\n{ }",
            // The shapes that must keep reading: an ordinary head, an ordinary argument, and a default with no
            // directive at all.
            "template<typename T, int N = 3, typename... R> void f();",
            "template<typename T = int> struct X { };",
            "std::map<K, std::less<>> m;",
            "X<1, 2> x;",
        ],
    );

    // The shape: the directive is a node of *the thing it was written inside* — the argument list, or the
    // parameter list — and the value it guards is still read as a value.
    let argument_list_holds = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        tree.get_red_root()
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
            .map(|list| {
                (
                    list.descendants()
                        .filter(|node| {
                            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::PreprocessorDirective
                        })
                        .count(),
                    list.descendants()
                        .filter(|node| {
                            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument
                        })
                        .count(),
                )
            })
            .unwrap_or_default()
    };

    assert_eq!(
        argument_list_holds("X<\n#if A\n  1\n#else\n  2\n#endif\n  > x;"),
        (3, 2),
        "the three directives are the argument list's, and **each branch contributes its own argument** — the \
         list is what the conditional spells twice"
    );

    // **The tail per branch reads now**, and it is the *list* that reads it: a `>` closes this list in one
    // spelling, and when the `;` after it is followed by an `#else`/`#elif` — and this list opened that
    // conditional inside itself — the branch's own tail is read as the same list. The claim is the one every
    // spelling of the seam makes: one list, every spelling in it, and the directives are its own nodes.
    let source = "template<typename I>\n  using K = remove_const_t<\n#if X\n    tuple_element_t<0, T>>;\n#else\n    \
                  typename I::first_type>;\n#endif";
    assert_reads(Where::File, &[source]);
    assert_eq!(
        argument_list_holds(source),
        (2, 4),
        "one argument list: the `#if` and the `#else` are its own, and each branch contributes its own arguments \
         (`tuple_element_t<0, T>` with its two nested ones, and the second branch's one). The `#endif` stands after \
         the `;` that ends the declaration, so it is the enclosing declaration's directive, not the list's"
    );
}

/// **A deduction guide is a declarator with a trailing return type** — `M(I) -> M<I>;`.
///
/// C++17's deduction guide is written as a template head, then what looks like a call, then `-> type` — and the
/// `->` is the whole evidence, because a **trailing return type belongs to a function declarator and to nothing
/// else**: a variable's initializer cannot be followed by one. Without that, the preference for the initializer
/// reading (which is right for `Widget w(T)`, and exists for `Max(a, b);`) took the group for a
/// direct-initialisation and the `->` for what came after it:
///
/// ```cpp
///   template<typename _InputIterator, typename _Allocator, typename = …>
///     multimap(_InputIterator, _InputIterator, _Allocator)      // bits/stl_multimap.h:1153
///     -> multimap<__iter_key_t<_InputIterator>, __iter_val_t<_InputIterator>,
///                 less<__iter_key_t<_InputIterator>>, _Allocator>;
/// ```
///
/// Four files of the closure had one of these as their first error (`bits/map`, `bits/multimap`,
/// `bits/stl_multimap.h`, `string_view`), and all four are clean now.
///
/// The negative half matters more than usual here, because `->` is also the **member access** operator: an
/// expression like `(a)->b` is not a declaration, and the reading below must not turn it into one.
#[test]
fn a_deduction_guide_is_a_declarator_with_a_trailing_return_type() {
    assert_reads(
        Where::File,
        &[
            "template<typename I> M(I) -> M<I>;",
            "template<typename I> M(I, I) -> M<int>;",
            "template<typename I>\n  M(I, I)\n  -> M<K<I>, V<I>>;",
            "template<typename I>\n  multimap(I, I)\n  -> multimap<K<I>, V<I>,\n              less<K<I>>, A>;",
            // The real one, spelled out.
            "template<typename _InputIterator, typename _Allocator, typename = _RequireInputIter<_InputIterator>>\n\
             \x20 multimap(_InputIterator, _InputIterator, _Allocator)\n\
             \x20 -> multimap<__iter_key_t<_InputIterator>, __iter_val_t<_InputIterator>,\n\
             \x20             less<__iter_key_t<_InputIterator>>, _Allocator>;",
            "template<typename _It> basic_string_view(_It, _It) -> basic_string_view<iter_value_t<_It>>;",
            // What must keep reading: an ordinary trailing return type, a variable with an initializer, and the
            // arrow used as the member-access operator inside an expression.
            "auto f(int) -> int;",
            "struct S { auto f() -> int { return 1; } };",
            "void f() { auto r = (a)->b; }",
            "void f() { auto l = []() -> int { return 1; }; }",
            "void f() { T x(y); }",
        ],
    );

    // The shape: the group is a **parameter list** and the `->` its trailing return type, with no initializer in
    // sight — and the member-access expression keeps its own reading.
    let parts = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        let root = tree.get_red_root();
        let count = |kind| {
            root.descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
                .count()
        };
        (
            count(CppSyntaxKind::ParameterList),
            count(CppSyntaxKind::TrailingReturnType),
            count(CppSyntaxKind::Initializer),
        )
    };

    assert_eq!(
        parts("template<typename I> M(I) -> M<I>;"),
        (1, 1, 0),
        "a guide is a function declarator: one parameter list, one trailing return type, no initializer"
    );
    assert_eq!(
        parts("void f() { auto r = (a)->b; }"),
        (1, 0, 1),
        "and `(a)->b` is an initializer — the outer function's parameter list, and no trailing return type"
    );
}

/// **An `asm` statement**, whose payload is the compiler's language rather than C++.
///
/// `asm` is not a C++ keyword (it is C's, and an extension spelled the same way by GCC and by MSVC), so this
/// lexer produces an ordinary identifier and the shape is what claims the statement. The payload is the point:
///
/// ```cpp
/// __asm__ volatile ("tilerelease" ::);                       // amxtileintrin.h:56
/// __asm__ __volatile__("int {$}3":);                         // _mingw.h:584
/// __asm__ __volatile__ ("pconfig\n\t" : "=a" (retval) : "a" (leaf) : "cc");
/// ```
///
/// `"int {$}3":` and `"a" (leaf)` are GCC's operand language — no expression rule can read them, and inventing one
/// would both lose the text and be wrong about what is there. So the payload is kept **as tokens** inside one
/// [`CppSyntaxKind::AsmStat`] node, which is what a consumer wants: a highlight, a hover, an asm block moved as a
/// unit. Both the empty second operand section (`::`) and the empty third (`:`) are in the corpus, which is why
/// the rule reads a *balanced group* rather than anything with structure.
///
/// The negative half is the spelling evidence: a name followed by a parenthesised group is otherwise a **call**,
/// and the difference is the three spellings. A file that `#define`s `asm` is asking for the macro rules, and it
/// gets them.
#[test]
fn an_asm_statement_keeps_its_payload_as_tokens() {
    assert_reads(
        Where::Body,
        &[
            "__asm__ volatile (\"tilerelease\" ::);",
            "__asm__ __volatile__(\"int {$}3\":);",
            "__asm__ __volatile__ (\"pconfig\\n\\t\" : \"=a\" (retval) : \"a\" (leaf) : \"cc\");",
            "__asm__ (\"nop\");",
            "asm(\"nop\");",
            "asm volatile (\"dmb\" ::: \"memory\");",
            "asm goto (\"jmp %l0\" :::: label);",
            "__asm__ __volatile__ (\"pconfig\\n\\t\"\t\\\n\t: \"=a\" (retval)\t\t\t\\\n\t: \"a\" (leaf), \"b\" (b)\t\t\\\n\
             \t: \"cc\");",
            // MSVC's spelling: a block, and no `;` after it.
            "__asm { mov eax, 1 }",
            // What must keep reading as it did: a call, and a name that only *looks* similar.
            "g(1, 2);",
            "asmbl(1);",
        ],
    );

    // The shape: one node holding the tokens in order — including the ones no rule could read.
    let tree = CppParser::parse(
        "void f() { __asm__ volatile (\"pconfig\\n\\t\" : \"=a\" (retval) : \"a\" (leaf) : \"cc\"); }",
        ParserConfig::default(),
    );
    let root = tree.get_red_root();
    let asm = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AsmStat)
        .expect("the statement is an asm statement");
    let text = asm.text().to_string();
    assert!(
        // `trim_end`, because the trivia *after* the `;` belongs to the enclosing node as well — the tree keeps
        // every byte, and a node's text is allowed to end in the space before the next one.
        text.starts_with("__asm__ volatile (") && text.trim_end().ends_with(");"),
        "the node spans the whole statement: {text:?}"
    );
    assert!(
        text.contains("\"=a\" (retval)") && text.contains("\"cc\""),
        "and every operand is still in it, as text: {text:?}"
    );
    assert!(
        !root.descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode
        }),
        "nothing in an asm statement is rubble"
    );

    // …and a file whose own `#define` claims the name keeps the macro reading.
    let tree = CppParser::parse(
        "#define asm(x) g(x)\nvoid f() { asm(1); }",
        ParserConfig::default(),
    );
    assert!(
        !tree
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AsmStat),
        "a name this file defines as a macro is not the compiler's keyword"
    );
}

/// **The cast reading is a preference, and a group that holds a call is an expression.**
///
/// `(T)…` is read as a C-style cast on evidence — the name inside the parentheses is a type this file knows —
/// and that evidence is *also* true of a **functional conversion used as a value inside a grouped expression**,
/// which is what a template parameter looks like everywhere in libstdc++'s math implementations:
///
/// ```cpp
/// __gam1 = (__gammi - __gampl) / (_Tp(2) * __mu);                          // tr1/bessel_function.tcc:114
/// __fact *= __k / (_Tp(2) * __numeric_constants<_Tp>::__pi());             // tr1/gamma.tcc:117
/// static const _CASable _CASable_mask = ((_CASable(1) << (_CASable_bits / 2)) - 1);
/// _Tp __p_lm = (_Tp(2 * __j - 1) * __x * __P_lm1m …);                      // legendre_function.tcc:175
/// ```
///
/// There the type-id is `_Tp` and a `(` follows, so the `)` the cast demands never comes: the declaration came
/// out as rubble with `expected ), but get (`, and **twelve files** of the closure had that as their first error
/// (the nine `.tcc` math implementations, `parallel/types.h`, `bits/stl_bvector.h`, `bits/max_size_type.h`,
/// `bits/type_traits.h`).
///
/// The guard's own comment already promised the answer — "a cast whose operand fails to parse is rewound and
/// read as a parenthesised expression" — and what was missing was the rewind for the *type* half of the attempt.
/// A checkpoint taken before the cast does it: the failed type reading and its diagnostics disappear
/// ([`CppParser::rollback`] truncates both), and the expression rule gets the tokens.
///
/// The negative half is what a cast is: `(T)x`, `(int)x` and the abstract-declarator spellings
/// (`(T(*)(int))x`) must all still come out as `CastExpr`.
#[test]
fn a_group_holding_a_call_is_an_expression_not_a_cast() {
    assert_reads(
        Where::Body,
        &[
            "auto r = (T(2) * c);",
            "auto r = (a - b) / (T(2) * c);",
            "z = (a - b) / (T(2) * c);",
            "auto r = ((T(1) << (n / 2)) - 1);",
            "auto r = (T(2 * j - 1) * x * y);",
            "auto r = (x + T(1)) / (T(2) * y);",
            "auto r = (T(2));",
            // The same, with the template parameter the real files use.
            "template<typename T>\n  void f() { T x = (a - b) / (T(2) * c); }",
            "template<typename _Tp>\n  _Tp g(_Tp a, _Tp b, _Tp c)\n  {\n    _Tp x;\n    x = (a - b) / (_Tp(2) * c);\n    \
             return x;\n  }",
            // …and what a cast is, which must keep reading as one.
            "auto r = (T)x;",
            "auto r = (int)x;",
            "auto r = (T(*)(int))x;",
            "auto r = (const T&)x;",
            "auto r = (a + b);",
        ],
    );

    // **Which of the two readings came out**, and that the failed attempt left nothing behind.
    let kinds = |source: &str| {
        let tree = CppParser::parse(&format!("void probe() {{ {source} }}"), ParserConfig::default());
        let root = tree.get_red_root();
        let found = |kind| {
            root.descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == kind)
        };
        (found(CppSyntaxKind::CastExpr), found(CppSyntaxKind::CallExpr))
    };

    assert_eq!(kinds("auto r = (T)x;"), (true, false), "`(T)x` is a cast");
    assert_eq!(
        kinds("auto r = (T(*)(int))x;"),
        (true, false),
        "…and so is one whose type has an abstract declarator"
    );
    assert_eq!(
        kinds("auto r = (T(2) * c);"),
        (false, true),
        "`(T(2) * c)` is a product, and `T(2)` inside it is a call"
    );
    assert_eq!(
        kinds("auto r = (T(2));"),
        (false, true),
        "…and a lone `(T(2))` is the same call, not a cast of `2` to `T`"
    );

    // The **diagnostics** of the abandoned attempt are gone with it: a problem with a reading nobody kept is a
    // problem the file does not have.
    assert!(
        CppParser::parse(
            "void probe() { auto r = (T(2) * c); }",
            ParserConfig::default()
        )
        .get_errors()
        .is_empty(),
        "the cast attempt reported `expected ), but get (` — the rewind must take that back"
    );
}

/// **A parameter list is written once per branch, or has a directive between its parameters.**
///
/// A `#` at a parameter position cannot be anything else — a parameter begins with a type, or with a directive —
/// and the same is true one line up, between the parameters. `parallel/algorithmfwd.h:700` is the spelling that
/// put the seam in:
///
/// ```cpp
///     random_shuffle(_RAIter, _RAIter,
/// #if __cplusplus >= 201103L
///            _RandomNumberGenerator&&);
/// #else
///            _RandomNumberGenerator&);
/// #endif
/// ```
///
/// Three readings came out of one seam: a directive **before** a parameter (the loop that reads them), a
/// parameter list **split** by one, and a parameter **spelled once per branch** — an `#else`/`#elif` at that
/// position writes *this* parameter the other way, so another parameter follows rather than a new one starting.
/// The two are told apart by the directive's **name**, and the measurement is what says the name is the evidence:
/// `#endif` only closes something, and the `#else` that ends the *first branch's whole declaration* in
/// `parallel/algorithmfwd.h` closes one too.
///
/// **The fourth reading — the list's tail per branch — was B65 and is now closed.** That file's second branch is a
/// *fragment* (`_RandomNumberGenerator&);`: the head is above the `#if`), and what reads it is the list itself: a
/// `)` is the end of the list *in one spelling*, and when the `;` after it is followed by an `#else`/`#elif` **and**
/// the list opened that conditional inside itself, the branch's own tail is read as the same list. The two
/// conditions matter together — without the second one, `#if X void f(int a); #else void g(long a); #endif` is read
/// as one list with `void g(long a)` as parameters, which is the regression the corpus showed as `type_traits:1177`
/// and `stl_iterator.h:3023`.
#[test]
fn a_directive_may_decide_a_parameter_or_stand_between_two() {
    assert_reads(
        Where::File,
        &[
            // A directive between two parameters.
            "void f(int a,\n#if X\n  int b,\n#endif\n  int c);",
            // …and one before the first of them.
            "void f(\n#if X\n  int a,\n#endif\n  int b);",
            // The parameter list split by a directive, with the terminator inside the branch is **not** here:
            // `parallel/algorithmfwd.h:700` writes its second branch as a fragment, and that is B65 (pinned
            // below).
            // A parameter written once per branch: the `#else` spells *this* parameter another way.
            "void f(int a,\n#if X\n  int b\n#else\n  long b\n#endif\n  );",
            "void f(int a,\n#if X\n  int b\n#elif Y\n  long b\n#else\n  short b\n#endif\n  );",
            // The same shape in a definition, where the body follows the list.
            "void f(int a,\n#if X\n  int b\n#else\n  long b\n#endif\n  ) { }",
        ],
    );

    // **One list, every parameter in it**: the seam must not end the list and re-open one per branch. A second
    // `ParameterList` (or a missing `Parameter`) is exactly the silent wrong tree this test exists to catch.
    let parameters = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        let root = tree.get_red_root();
        let lists: Vec<_> = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ParameterList)
            .collect();
        assert_eq!(lists.len(), 1, "one parameter list, not one per branch");
        lists[0]
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Parameter)
            .count()
    };

    assert_eq!(
        parameters("void f(int a,\n#if X\n  int b,\n#endif\n  int c);"),
        3,
        "the parameter between two directives is a parameter"
    );
    assert_eq!(
        parameters("void f(int a,\n#if X\n  int b\n#else\n  long b\n#endif\n  );"),
        3,
        "two parameters written with an alternation are `int a`, `int b` and `long b`: the parser has no table \
         that says the branches are exclusive, so each **spelling** is a parameter and keeping both is the honest \
         tree. What would be wrong is a second list, or the `long b` disappearing."
    );

    // **B65's fragment reads now**, and the claim is the same one every spelling of this seam makes: one list,
    // every spelling of every parameter in it.
    assert_reads(
        Where::File,
        &["void random_shuffle(_RAIter, _RAIter,\n#if X\n  _RandomNumberGenerator&&);\n#else\n  \
           _RandomNumberGenerator&);\n#endif"],
    );
    assert_eq!(
        parameters(
            "void random_shuffle(_RAIter, _RAIter,\n#if X\n  _RandomNumberGenerator&&);\n#else\n  \
             _RandomNumberGenerator&);\n#endif"
        ),
        4,
        "one list, and both branches' spellings of the last parameter in it"
    );

    // The gate's **second** condition, which is what keeps a declaration written per branch out of this reading:
    // the `#else` here belongs to a conditional opened *outside* the list, so the `;` before it is the
    // declaration's and the next branch's text is a declaration rather than more parameters.
    assert_reads(
        Where::File,
        &["#if X\nvoid f(int a);\n#else\nvoid g(long a);\n#endif"],
    );
    let source = "#if X\nvoid f(int a);\n#else\nvoid g(long a);\n#endif";
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        2,
        "each branch writes a whole declaration, so each has its own list"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::Parameter),
        2,
        "and one parameter in each — the `;` before the `#else` was the declaration's, not the list's"
    );
}

/// **A macro's arguments are tokens, not expressions — and a call whose arguments do not read is where that
/// shows.**
///
/// A macro written in an *included* header is in no table this parser is handed, and its arguments are pasted
/// into types, qualifiers and operators alike:
///
/// ```cpp
/// if constexpr (__is_same(const volatile _Tp, const volatile void))     // bits/new:234
/// return __reference_constructs_from_temporary(_Elements, _Up&&) …;     // tuple:922
/// _GLIBCXX_TYPEID(typename std::iterator_traits<_Iterator>::value_type); // bits/formatter.h:485
/// _MM_REDUCE_OPERATOR_BASIC_EPI16 (+);                                   // avx512vlbwintrin.h:4992
/// ```
///
/// Every one of those is a *call* — the name, the parentheses, the arguments — and not one argument is an
/// expression. Measured before the fallback, on the 455-file closure: **nine files** had one of these as their
/// first error (`avx512vlbwintrin.h`, `bits/stl_pair.h`, `bits/formatter.h`, `limits`, `new`, `gthr-default.h`,
/// `emmintrin.h`, `xmmintrin.h`, `shellapi.h`), and `CHANGELOG`-style declarations like
/// `WINOLEAPI_(void) CoUninitialize (void);` went with them, because the macro's `(void)` is read as arguments
/// too. All nine became clean, and no file on either closure changed from clean to failing.
///
/// The negative half is the price, and it is pinned here rather than left implicit: **a genuinely broken call is
/// read as a macro's arguments and reported by nobody.** No shape separates the two — the arguments of a macro
/// *are* arbitrary tokens — so the choice is between the diagnostic on `g(1 +)` and every use of every macro from
/// every header. B70 in `docs/grammar-gaps.md` records it as a cost rather than as a gap.
#[test]
fn a_macros_arguments_that_are_not_expressions_stay_tokens() {
    assert_reads(
        Where::Body,
        &[
            "_MM_REDUCE_OPERATOR_BASIC_EPI16 (+);",
            "if constexpr (__is_same(const volatile _Tp, const volatile void)) { }",
            "return __reference_constructs_from_temporary(_Elements, _Up&&);",
            "_GLIBCXX_TYPEID(typename std::iterator_traits<_Iterator>::value_type);",
            "if (TlsSetValue (__key, CONST_CAST2(void *, const void *, __ptr))) { }",
            "auto n = __glibcxx_min(char);",
            // The same shape where the macro's argument is a *cast*: `(__attribute__((__vector_size__ (16))) int)`.
            "auto v = __builtin_shuffle ((__attribute__((__vector_size__ (16))) int) __A, __B);",
        ],
    );

    // The neighbouring shape — a **declaration whose head is such a macro** — used to be the boundary of this
    // rule and is now read by the one that owns it: `WINOLEAPI_(void) CoUninitialize (void);` is a declaration
    // whose specifiers are a macro invocation (`docs/grammar-gaps.md` B72), and it is asserted there.
    assert_reads(
        Where::File,
        &["SHSTDAPI_(WINBOOL) InitNetworkAddressControl (void);"],
    );

    // **Which reading came out.** An argument list that did not read is kept as one `ArgumentList` of raw tokens;
    // the one that did read has no such node, because its arguments are the expressions they were written as.
    let arguments_are_tokens = |statement: &str| {
        let source = format!("void probe() {{ {statement} }}");
        let tree = CppParser::parse(&source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{statement}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );

        tree.get_red_root().descendants().any(|node| {
            matches!(
                CppSyntaxKind::from(node.kind()),
                CppSyntaxKind::CallExpr
            ) && node.children().any(|child| {
                CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList
            })
        })
    };

    assert!(
        arguments_are_tokens("_MM_REDUCE_OPERATOR_BASIC_EPI16 (+);"),
        "an argument that is a bare operator is kept as a token group"
    );
    assert!(
        !arguments_are_tokens("g(1, 2);"),
        "an ordinary call's arguments are expressions, and an `ArgumentList` node here would say they are not"
    );
    assert!(
        !arguments_are_tokens("g(h(x), {1, 2});"),
        "…including the braced-init-list the call arm already reads"
    );

    // The price, on purpose: `g(1 +)` is not read as an expression and is **not** reported. Pinned so that a
    // later narrowing of the fallback fails here first, which is where the reason for it is written down.
    assert!(
        arguments_are_tokens("g(1 +);"),
        "a broken call is read as a macro's arguments — the documented cost of B70"
    );
}

/// **`using enum E;` and `register` — two spellings the grammar had no rule for at all.**
///
/// Neither is a question about *which* reading: `using enum _Fp_fmt;` (`compare:710`) was reported as
/// `expected a name` because the `using` rule read the keyword `enum` as the name being introduced, and a
/// `register` at the start of a declaration (`_mingw.h:607`, `register unsigned int r0 __asm__("r0") = code;`)
/// was not in the list of tokens a declaration may start with — so the statement reader never asked the
/// declaration question and read `register` as an expression.
///
/// C++20's using-enum-declaration introduces the enum's **enumerators**, not a type name, so nothing is recorded
/// about it: the tree is a `UsingDecl` holding the `enum` keyword and the name. `register` is a storage-class
/// specifier and gets the storage-class treatment — a node of its own (`RegisterSpec`, declared **last** among
/// the kinds so that no stored discriminant changes meaning) produced by the same table `static` and `mutable`
/// come from, and an entry in `can_begin_a_declaration`.
///
/// The third shape below came along with the second and is pinned rather than claimed: a **GNU asm label** on a
/// declarator (`r0 __asm__("r0")`) has no C++ grammar behind it — like the `asm` *statement* of B67 — so it is
/// read as a `MacroCall` with every token kept. That is a tolerant reading and not a declaration of what it is;
/// pinning it means a later node of its own fails here first, which is where the reason will be written down.
#[test]
fn a_using_enum_declaration_and_the_register_specifier_read() {
    // `using enum` in all three places a using-declaration may stand, and with a qualified name.
    assert_reads(
        Where::File,
        &[
            "enum class E { a };\nusing enum E;",
            "namespace ns { enum class E { a }; }\nusing enum ns::E;",
            "enum class E { a };\ntemplate<class T> void f() { using enum E; }",
        ],
    );
    assert_reads(
        Where::Body,
        &["using enum E;", "using enum ns::E;"],
    );
    assert_reads(Where::Class, &["using enum E;"]);

    // `register` where a declaration may stand, alone and among other specifiers.
    assert_reads(
        Where::Body,
        &[
            "register int x = 0; (void)x;",
            "register const char *p = nullptr; (void)p;",
            "for (register int i = 0; i < 3; ++i) { }",
        ],
    );
    assert_reads(
        Where::File,
        &[
            "register int counter;",
            "register unsigned int r0 __asm__(\"r0\") = code;",
        ],
    );

    // **Which reading came out**, in both halves. A `RegisterSpec` inside the specifier sequence — and the
    // `using enum` is a `UsingDecl` whose name is the enum's, not a declaration of `enum` as a name.
    let specifier = |source: &str, kind: CppSyntaxKind| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        tree.get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == kind)
    };

    assert!(
        specifier("void probe() { register int x = 0; }", CppSyntaxKind::RegisterSpec),
        "`register` is a storage-class specifier, so it gets a specifier node"
    );
    assert!(
        specifier("enum class E { a };\nusing enum E;", CppSyntaxKind::UsingDecl),
        "`using enum E;` is a using-declaration"
    );
    assert!(
        !specifier("enum class E { a };\nusing enum E;", CppSyntaxKind::MissingNode),
        "and nothing in it is missing"
    );

    // The asm label, as tokens under the macro reading (see the test's documentation).
    let label = CppParser::parse(
        "register unsigned int r0 __asm__(\"r0\") = code;",
        ParserConfig::default(),
    );
    assert!(
        label.get_errors().is_empty(),
        "the asm label on a declarator is not an error: {:?}",
        label.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        label.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall
        }),
        "…and its tokens are kept, under the macro reading, with nothing interpreted"
    );
}

/// **A macro may stand between the type and the declarator** — and the same three tokens mean the opposite when
/// the macro is a *suffix*.
///
/// ```cpp
/// void HUGEP **ppvData                  // windef.h's macro, in every COM signature (oleauto.h:71)
/// unsigned __int64 POINTER_64_INT;      // basetsd.h:11, corecrt.h:35
/// unsigned __int64 x;                   // …and the same shape with an ordinary declarator
/// int x MY_DECL_SUFFIX;                 // the *other* reading of `Type Name Name`: `x` declares, the macro is a suffix
/// ```
///
/// The specifier sequence let a name join a type only when the type already written was a **name** — the
/// `MY_API Widget *p;` shape — so `unsigned __int64 x;` was read as the type `unsigned`, a declarator named
/// `__int64`, and `x` as a `MacroCall` standing for a declaration: no diagnostic, no `ErrorNode`, every token in
/// the tree, and the wrong construct (an A0-class wrong tree, and `__int64` is a MinGW typedef, so this is how
/// most of the Windows headers write a 64-bit variable).
///
/// Relaxing that condition to "a type has been named" fixed the three shapes above and **broke the fourth** —
/// the two are the same tokens with opposite meanings, and two existing tests caught it: the variable's name was
/// lost (`int x MY_DECL_SUFFIX;` read as a type `int x` with a declarator named `MY_DECL_SUFFIX`) and B71's asm
/// label stopped being a macro. What separates them is the **spelling of the name that joins**: `__int64`,
/// `HUGEP`, `MY_API` are written the way a macro is, `x` is not. See `types::written_like_a_macro` — a convention
/// used here only to choose between two readings that both occur, which is the same last-resort role
/// `decls::looks_like_a_macro_name` documents.
#[test]
fn a_macro_may_stand_between_the_type_and_the_declarator() {
    assert_reads(
        Where::File,
        &[
            "unsigned __int64 x;",
            "typedef unsigned __int64 POINTER_64_INT;",
            "signed __int64 y;",
            "void f(void HUGEP **ppvData);",
            "WINOLEAUTAPI SafeArrayAccessData(SAFEARRAY *psa, void HUGEP **ppvData);",
            "unsigned __int64 f(unsigned __int64 a);",
            // A **macro invocation as the whole declaration head**, with the declarator after it (B72's second
            // half): `WINOLEAPI_` expands to `EXTERN_C DECLSPEC_IMPORT type STDAPICALLTYPE`.
            "WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);",
            "WINOLEAPI_ (void) OleUninitialize (void);",
            "MY_API(x) int g(void);",
            // **Not** here, and it is the boundary of this rule: `STDMETHOD(QueryInterface) (THIS_ REFIID riid,
            // LPVOID *ppvObj) PURE;` (`commdlg.h:577`). Its declarator has **no name** — the name is inside the
            // macro's own argument list (`#define STDMETHOD(method) virtual HRESULT STDMETHODCALLTYPE method`) — so
            // the parameter list that follows the macro belongs to a name this layer cannot see. Reading it as a
            // declaration would declare a function with no name; the shape is left to B72's third item.
            // The shapes that were already read, which must keep reading.
            "MY_API Widget *p;",
            "MY_API Widget const w;",
            // **The false positive this rule had to grow a guard for**: `__attribute__((…))` is also a name, a
            // balanced group and then an identifier, and it has a reader that knows what it is. Reading it as a
            // macro specifier ended the specifier sequence at the attribute, so the declaration below had no type
            // and `bits/stl_tree.h` — clean before this rule — reported `expected ;` in the middle of it.
            "__attribute__((__nonnull__)) void f(const bool __insert_left);",
            "__attribute__((__nonnull__,__returns_nonnull__))\n  _Rb_tree_node_base*\n  _Rb_tree_rebalance_for_erase(_Rb_tree_node_base* const __z,\n                               _Rb_tree_node_base& __header) throw ();",
            "__declspec(align(8)) int aligned;",
            "_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR bool empty() const;",
            "int x;",
            "T x;",
            "int a, b;",
        ],
    );
    assert_reads(
        Where::Body,
        &[
            "unsigned __int64 n = 0; (void)n;",
            "void *HUGEP p = nullptr; (void)p;",
        ],
    );

    // **Which reading came out.** A specifier sequence of two words, no `MacroCall` anywhere in the declaration,
    // and the declarator is the *second* name — for the macro-between-type-and-declarator shape; and for the
    // suffix shape the exact opposite on all three counts.
    let shape = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let root = tree.get_red_root();
        let specifiers = root
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DeclSpecifierSeq)
            .map(|sequence| sequence.text().to_string().trim().to_string())
            .unwrap_or_default();
        let macros = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count();
        let declarator = root
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)
            .map(|node| node.text().to_string().trim().to_string())
            .unwrap_or_default();
        (specifiers, macros, declarator)
    };

    assert_eq!(
        shape("unsigned __int64 x;"),
        ("unsigned __int64".to_string(), 0, "x".to_string()),
        "the macro is a word of the *type*, and `x` is the declarator — the reading that was silently wrong"
    );
    assert_eq!(
        shape("int x MY_DECL_SUFFIX;"),
        ("int".to_string(), 1, "x".to_string()),
        "…and here the macro is a suffix: the declarator is `x` and the macro is its own node"
    );
    assert_eq!(
        shape("MY_API Widget *p;"),
        ("MY_API Widget".to_string(), 0, "*p".to_string()),
        "the shape the rule was written for is unchanged: the macro is in the type"
    );

    // **Who the declaration names.** A macro standing for the specifiers must not become the declared name: the
    // declarator after it does. `WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);` declares `CoFreeLibrary` — a
    // function with a parameter list — and the macro is a `MacroCall` inside the specifier sequence, with its
    // argument list kept as tokens. Read the other way round, the file would declare a function called
    // `WINOLEAPI_` and `CoFreeLibrary` would be rubble.
    let declared = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        tree.get_red_root()
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
            .map(|node| node.text().to_string().trim().to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        declared("WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);"),
        "CoFreeLibrary (HINSTANCE hInst)".to_string(),
        "the declarator after the macro is the declaration's declarator"
    );
    assert_eq!(
        declared("WINOLEAPI_ (void) OleUninitialize (void);"),
        "OleUninitialize (void)".to_string(),
        "…including the spelling with a space before the macro's parenthesis"
    );
}

/// **The same macro-shaped name, one level down: inside a type-id, and inside a parenthesised declarator.**
///
/// B72 and B73 put a macro-shaped name between a type and a declarator. The same spelling turns up in two other
/// places, and each has its own reason:
///
/// ```cpp
/// static __inline unsigned __LONG32 HandleToULong (const void *h)              // basetsd.h:68
/// { return ((unsigned __LONG32) (ULONG_PTR) h); }                              // the cast's *type-id*
/// typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);                                 // winperf.h:180
/// typedef DWORD (WINAPI PM_COLLECT_PROC)(LPWSTR,LPVOID *,LPDWORD,LPDWORD);     // winperf.h:181
/// ```
///
/// A **type-id** has no declarator, so `allow_second_name` is `false` there — which is right for the question it
/// was written for (a second *name* would run `template <typename T, typename U>` together) and wrong for a name
/// spelled the way an unexpanded type is spelled: `(unsigned __LONG32)` is a cast, and refusing the name left the
/// cast unreadable and every one of those inline functions broken. A **parenthesised declarator** has the macro
/// *before* the name — `(WINAPI PM_OPEN_PROC)`, where `WINAPI` is `__stdcall` — and the group is followed by the
/// parameter list that belongs to the declarator.
///
/// The third assertion is the one that matters most, because the first version of the declarator half broke it:
/// `void C::f(_Predicate __pred) { }` has the *identical* group `(IDENTIFIER IDENTIFIER)`, and claiming it as a
/// parenthesised declarator turned a member definition into rubble — the body's declarations landed outside it and
/// the error surfaced on a `typedef` three lines further down (`debug/safe_sequence.tcc`, which had been clean).
/// What separates the two is the **follower**: a parameter list is never followed by another `(` belonging to the
/// same declarator, and the function-pointer typedef always is.
#[test]
fn a_macro_shaped_name_inside_a_type_id_and_a_parenthesised_declarator() {
    assert_reads(
        Where::Body,
        &[
            "auto v = ((unsigned __LONG32) h);",
            "auto v = ((void *) (LONG_PTR) (__LONG32) h);",
            "auto n = sizeof(unsigned __LONG32);",
            "auto p = (unsigned __int64 *) h;",
        ],
    );
    assert_reads(
        Where::File,
        &[
            "typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);",
            "typedef DWORD (WINAPI PM_COLLECT_PROC)(LPWSTR,LPVOID *,LPDWORD,LPDWORD);",
            "typedef DWORD (WINAPI PM_CLOSE_PROC)(void);",
            "static int HandleToULong (const void *h) { return ((unsigned __LONG32) h); }",
            // …and the shapes that share the tokens, which must keep their own readings.
            "void C::f(_Predicate __pred) { }",
            "void C::g(_Predicate __pred);",
            "void f(int (_Predicate __pred));",
            "typedef void (*fp)(int);",
            "int x MY_DECL_SUFFIX;",
        ],
    );

    // **A parameter list stays a parameter list.** The group `(_Predicate __pred)` is the shape the declarator
    // half claims when another `(` follows, so this is where a mis-claim would show: the parameter's name must be
    // `__pred` and the group must still be a `ParameterList`.
    let parameter_list = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let root = tree.get_red_root();
        let parameters = root
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ParameterList)
            .map(|list| {
                list.descendants()
                    .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Parameter)
                    .map(|parameter| parameter.text().to_string().trim().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let macros = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count();
        (parameters, macros)
    };

    assert_eq!(
        parameter_list("void C::f(_Predicate __pred) { }"),
        (vec!["_Predicate __pred".to_string()], 0),
        "a parameter list with a macro-shaped type and a name is a parameter list"
    );
    assert_eq!(
        parameter_list("void C::g(_Predicate __pred);"),
        (vec!["_Predicate __pred".to_string()], 0),
        "…and the same in a declaration"
    );

    // **The macro before the name is a `MacroCall`, and the name after it is the declarator's.** The group is
    // followed by the parameter list that belongs to that declarator, which is the follower that told the two
    // shapes apart in the first place.
    let declared = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let root = tree.get_red_root();
        let macros = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .map(|node| node.text().to_string().trim().to_string())
            .collect::<Vec<_>>();
        let declarator = root
            .descendants()
            .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)
            .map(|node| node.text().to_string().trim().to_string())
            .unwrap_or_default();
        (macros, declarator)
    };

    assert_eq!(
        declared("typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);"),
        (
            vec!["WINAPI".to_string()],
            "(WINAPI PM_OPEN_PROC)(LPWSTR)".to_string()
        ),
        "the calling convention is a macro, the name after it is the declarator's, and the parameter list binds \
         to that same declarator — which is what makes this a function-pointer typedef"
    );
}

/// **The name after an anonymous class definition is the declarator** — including when nothing follows it.
///
/// ```cpp
/// union
/// {
///   __m128h __a[2];
///   __m256h __v;
/// } __u = { .__v = __A };        // avx512fp16vlintrin.h:155
/// ```
///
/// A *named* definition writes a name, so the sequence has a type by the time the declarator arrives and
/// `has_type_specifier` decides correctly. An **anonymous** one never does: `union { … }` sets no flag, so the
/// early return in `name_joins_the_type` — "no type yet, so this name can only be the type" — took `__u` for a
/// word of the type, and the declaration came out as `union { … } __u` **with no declarator at all**.
///
/// Two faces, and the quiet one is worse: with no initializer there was **no diagnostic** — no error, no
/// `ErrorNode`, no `MissingNode`, every token in the tree, and a variable that is not declared; with one, the
/// `expected a declarator name` was reported against the `=` (because an initializer needs something to
/// initialise). The body is a complete type whatever the flags say, so the class-definition question is asked
/// **before** the "no type yet" return.
#[test]
fn a_name_after_an_anonymous_class_definition_is_the_declarator() {
    assert_reads(
        Where::Body,
        &[
            "union { int a; } u;",
            "union { int a; } u = { 1 };",
            "struct { T a; } x = { 1 };",
            "struct { __m128h a[2]; __m256h v; } __u = { .__v = __A };",
            "union { __m128h __a[2]; __m256h __v; } __u;",
            "struct { int a; } *p = nullptr;",
        ],
    );
    assert_reads(
        Where::File,
        &[
            "typedef struct { int a; } Alias;",
            "enum E { A } e;",
            "struct S { int a; } x;",
            "struct { int a; } arr[] = { { 1 }, { 2 } };",
            "static union { int a; float b; } value = { .b = 1.0f };",
        ],
    );

    // **Where the name ended up.** It is the declarator of an `InitDeclarator` — for the shape *with* an
    // initializer, which used to be reported, and for the silent one, which used to be accepted with the name
    // swallowed into the type. `MY_DECL_SUFFIX` after it must stay out of the way of both.
    let declarator = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let root = tree.get_red_root();
        // The **last** one: the first is the enclosing function's own declarator (`probe()`), and the one this
        // test is about is inside its body.
        let all: Vec<_> = root
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
            .map(|node| node.text().to_string().trim().to_string())
            .collect();
        all.last().cloned().unwrap_or_default()
    };

    assert_eq!(
        declarator("void probe() { union { int a; } u = { 1 }; }"),
        "u = { 1 }".to_string(),
        "the initializer's declarator is `u`"
    );
    assert_eq!(
        declarator("void probe() { union { int a; } u; }"),
        "u".to_string(),
        "…and with nothing after it, `u` is still the declarator — the silence was the defect"
    );

    // The type half: the definition is in the specifier sequence and the name is **not**.
    let tree = CppParser::parse(
        "void probe() { union { int a; } u = { 1 }; }",
        ParserConfig::default(),
    );
    let root = tree.get_red_root();
    let specifiers = root
        .descendants()
        .find(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DeclSpecifierSeq
                && node
                    .children()
                    .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::UnionDef)
        })
        .map(|node| node.text().to_string().trim().to_string())
        .unwrap_or_default();
    assert_eq!(
        specifiers, "union { int a; }",
        "the type is the definition, and `u` is not a word of it"
    );
}

/// **A macro from a header is a statement of its own**, and the three boundaries that keep it from eating
/// anything else.
///
/// ```cpp
/// __glibcxx_function_requires(_LessThanComparableConcept<_Tp>)     // bits/stl_algobase.h:237
/// //return __b < __a ? __b : __a;
/// if (__b < __a)                                                   // the next token cannot continue a call
///   return __b;
/// ```
///
/// libstdc++ defines these concept-requirement macros as **nothing at all**, so the invocation is a whole
/// statement with no `;` — and the name is in no table this parser is handed. What makes the reading available is
/// three things at once, and each was bought by something going wrong without it:
///
/// * the name is one the **implementation reserved** (`_`-leading). `FOO(x)` is spelled like a macro too, but it
///   is also how a user's own function is spelled, and a macro of theirs written in this file would be `#define`d
///   here — evidence this rule does not have;
/// * what follows the group **cannot continue the expression**: `if`, `#`, `}`, `return`, a following name, a
///   declaration's first word. A block is not in that set (`g(x) { }` keeps its error);
/// * the reading is asked for **after** the declaration reading — and when that reading *succeeds*, which it does
///   here (`NAME ( parameter )` is a function declaration), the `;` is what tells the two apart: a declaration has
///   one, this macro's body supplies it. The first version asked before the declaration attempt and took B73's
///   `WINOLEAPI_(void) f(…)` away from it.
#[test]
fn a_macro_from_a_header_can_be_a_statement_of_its_own() {
    assert_reads(
        Where::Body,
        &[
            "__glibcxx_function_requires(_Concept<T>) if (a < b) return;",
            "#if X\n  __glibcxx_function_requires(_Concept<T>)\n#endif\n  g();",
            // Two in a row: the second one's follower is whatever comes after both.
            "__glibcxx_function_requires(_ConvertibleConcept<A, B>) __glibcxx_function_requires(_ConvertibleConcept<B, A>) typedef int T1;",
            "__glibcxx_function_requires(_Concept<T>) return;",
        ],
    );
    assert_reads(
        Where::File,
        &[
            "class B { } _GLIBCXX11_DEPRECATED_SUGGEST(\"std::bind\");",
            "__glibcxx_function_requires(_Concept<T>) void g();",
        ],
    );

    // **The boundaries.** A call with its `;` missing is still a call when the name is not reserved, and a block
    // after a call is still the mistake it was.
    let reported = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        !tree.get_errors().is_empty()
    };
    assert!(
        reported("void f() { FOO(x) }"),
        "`FOO(x)` is not reserved: a call with its `;` missing is an error"
    );
    assert!(
        reported("void f() { g(x)\n  return; }"),
        "…and a lowercase name is not a macro either"
    );
    assert!(
        reported("void f() { g(x) { } }"),
        "a block after a call is the mistake the block form of this rule weighs"
    );

    // **B73's shape is untouched**, which is the ordering this rule had to learn: the macro is the declaration's
    // *specifier* there, not a statement of its own.
    let tree = CppParser::parse(
        "WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);",
        ParserConfig::default(),
    );
    assert!(tree.get_errors().is_empty(), "B73's shape still reads");
    assert!(
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration
        }) && !tree
            .get_red_root()
            .children()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "it is one declaration whose specifiers hold the macro — not a macro statement and a second declaration"
    );
}

/// **A directive between enumerators**, which is the same seam as the one in a parameter list.
///
/// ```cpp
///       bad_file_descriptor = EBADF,
/// #ifdef EBADMSG
///       bad_message = EBADMSG,
/// #endif
///       broken_pipe = EPIPE,          // x86_64-w64-mingw32/bits/error_constants.h:52
/// ```
///
/// A `#` where an enumerator should be is not an enumerator, so the directive is read as the node it is and the
/// question is asked again about the token after it. The seam sits **after the comma and before the enumerator**,
/// which is also where a directive that closes a branch lands, so one loop covers both spellings.
#[test]
fn a_directive_may_stand_between_enumerators() {
    assert_reads(
        Where::File,
        &[
            "enum E {\n  a = 1,\n#ifdef X\n  b = 2,\n#endif\n  c = 3\n};",
            "enum E {\n  a,\n#if X\n  b,\n#else\n  c,\n#endif\n  d\n};",
            "enum class E : int {\n  a = 1,\n#ifdef X\n  b = 2\n#endif\n};",
            "enum E { a = 1, b = 2 };",
        ],
    );

    // **Which enumerators the enum has**, because a seam that swallowed one would leave a clean tree with a member
    // missing — the defect the comma rule already cost this rule once.
    let enumerators = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "`{source}` is reported: {:?}",
            tree.get_errors().iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::EnumeratorDecl)
            .map(|node| node.text().to_string().trim().to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        enumerators("enum E {\n  a = 1,\n#ifdef X\n  b = 2,\n#endif\n  c = 3\n};"),
        vec!["a = 1".to_string(), "b = 2".to_string(), "c = 3".to_string()],
        "all three, with the directive between the second and the third"
    );
}

/// **A compound requirement may hold a braced temporary** — and the `{` that opens a *body* must stay a body.
///
/// ```cpp
/// template<typename _Tp> concept __reversable = requires(_Tp& __t)
///   {
///     { _Begin{}(__t) } -> bidirectional_iterator;      // bits/ranges_base.h:214
///   };
/// ```
///
/// Inside a constraint, a `{` after an expression is refused by the postfix rule: `requires C<T> { }` would
/// otherwise come out as the constraint `C<T>{}` with the body missing. But a `{` **inside a compound
/// requirement's own braces** cannot be that body — it is nested one level in, and the reading it gets is the
/// ordinary one for a `{` after an expression (a list-initialised temporary, which `(__t)` then calls). Read as the
/// requirement's closing brace instead, the rule reported `expected }, but get {` and every requirement after it in
/// the body became rubble: 24 diagnostics in `bits/ranges_base.h`, which is now clean.
#[test]
fn a_compound_requirement_may_hold_a_braced_temporary() {
    assert_reads(
        Where::File,
        &[
            "struct Fn { template<class T> int operator()(T) const; };\ninline constexpr Fn _Begin{};\ntemplate<class T> concept B = true;\ntemplate<class T> concept R = requires(T& t) { { _Begin{}(t) } -> B; };",
            "template<class T> concept B = true;\ntemplate<class T> concept R = requires(T& t) { { T{1} } -> B; };",
            "template<class T> concept B = true;\ntemplate<class T> concept R = requires(T& t) { { t.f() } noexcept -> B; };",
            "template<class T> concept B = true;\ntemplate<class T> concept R = requires(T& t) { { _Begin{}(t) } -> B; { _Begin{}(t) } -> B; };",
            // …and the body of a constrained declaration is still the body.
            "template<class T> concept C = true;\ntemplate<class T> void f(T t) requires C<T> { }",
            "template<class T> concept C = true;\nint g(T t) requires C<T> { return 0; }",
        ],
    );

    // **Which reading came out.** The braced temporary is an `InitListExpr` inside the requirement, and the
    // constrained function's body is a `CompoundStat` — not part of an initializer and not part of the constraint.
    let tree = CppParser::parse(
        "struct Fn { template<class T> int operator()(T) const; };\ninline constexpr Fn _Begin{};\ntemplate<class T> concept B = true;\ntemplate<class T> concept R = requires(T& t) { { _Begin{}(t) } -> B; };",
        ParserConfig::default(),
    );
    assert!(tree.get_errors().is_empty(), "no diagnostics");
    let kinds: Vec<CppSyntaxKind> = tree
        .get_red_root()
        .descendants()
        .map(|node| CppSyntaxKind::from(node.kind()))
        .collect();
    assert!(kinds.contains(&CppSyntaxKind::InitListExpr), "`_Begin{{}}` is a list-initialised temporary");
    assert!(kinds.contains(&CppSyntaxKind::Requirement), "…inside the requirement");

    let constrained = CppParser::parse(
        "template<class T> concept C = true;\ntemplate<class T> void f(T t) requires C<T> { }",
        ParserConfig::default(),
    );
    assert!(
        constrained
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat),
        "the body of a constrained function is a body, not an initializer"
    );

    // **Not weakened: the class head still has no room for a clause.** `struct S requires C<T> { };` is refused,
    // which is what keeps the body from being detached (see `tests/concepts.rs`).
    assert!(
        !CppParser::parse(
            "template<class T> concept C = true;\ntemplate<class T> struct S requires C<T> { int member; };",
            ParserConfig::default()
        )
        .get_errors()
        .is_empty(),
        "the refusal the constraint marker exists for is untouched"
    );
}

/// **A base may be a `decltype`**, and a template argument list may carry a directive on either side of its comma.
///
/// ```cpp
///   template<typename... _Bn>
///     struct __or_
///     : decltype(__detail::__or_fn<_Bn...>(0))          // type_traits:199 — a *computed* base
///     { };
///
///     using __is_signed_integer = __is_one_of<__remove_cv_t<_Tp>,
///           signed char, signed long long
/// #if defined(__GLIBCXX_TYPE_INT_N_0)
///           , signed __GLIBCXX_TYPE_INT_N_0                // type_traits:811 — a directive before the comma
/// #endif
/// ```
///
/// The base clause read only a **name**, so `decltype(…)` — the spelling every `__or_`/`__and_` in libstdc++ uses
/// to name a base it computes rather than writes — was reported as `expected a name` against the `:`, and the whole
/// class head went with it (13 diagnostics in `type_traits`). The argument list had a directive seam **in front of**
/// an argument, which never sees a directive that stands after one and before the comma — so both sides of the comma
/// need it, exactly as in the enumerator list (B77).
#[test]
fn a_base_may_be_a_decltype_and_an_argument_list_holds_directives() {
    assert_reads(
        Where::File,
        &[
            "namespace d { template<class T> int f(int); }\nstruct O : decltype(d::f<int>(0)) { };",
            "namespace d { template<class... B> int f(int); }\ntemplate<class... B> struct O : decltype(d::f<B...>(0)) { };",
            "struct S : public decltype(0) { };",
            // The ordinary base clauses, which must keep their readings.
            "struct B { };\nstruct D : public B { };",
            "struct B { };\ntemplate<class T> struct D : B, T { };",
            "struct B { };\ntemplate<class... T> struct D : B, T... { };",
            // The argument list with a directive on either side of its comma.
            "template<class T> struct O { };\ntemplate<class T> using A = O<\n  int\n#if X\n  , long\n#endif\n>;",
            "template<class T> struct O { };\ntemplate<class T> using A = O<\n  int,\n#if X\n  long\n#endif\n  >;",
        ],
    );

    // **What the base holds, and how many arguments there are.** A directive that swallowed an argument would
    // leave a clean tree with a member missing — the same defect the comma rule cost this list once already.
    let tree = CppParser::parse(
        "namespace d { template<class T> int f(int); }\nstruct O : decltype(d::f<int>(0)) { };",
        ParserConfig::default(),
    );
    assert!(tree.get_errors().is_empty(), "no diagnostics");
    assert!(
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BaseSpecifier
                && node
                    .descendants()
                    .any(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::TypeId)
        }),
        "the base specifier holds a type-id — `decltype(d::f<int>(0))` — rather than a bare name"
    );

    let list = CppParser::parse(
        "template<class T> struct O { };\ntemplate<class T> using A = O<\n  int\n#if X\n  , long\n#endif\n>;",
        ParserConfig::default(),
    );
    assert_eq!(
        list.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgument)
            .count(),
        2,
        "both arguments, with the directive between them"
    );
}

/// **A placement `new`'s initialiser may hold an expression** — and the parentheses that hold one are not a
/// function type.
///
/// ```cpp
///     ::new (std::__addressof(_M_alloc)) _NodeAlloc(__nh._M_alloc.release());   // bits/node_handle.h:157
/// ```
///
/// The group after the type begins with a name, so `a_parameter_list_is_the_type` — which asked only about each
/// element's *first* token — claimed it as a **function type**, and the parameter reader then met the `.` and
/// reported `expected ), but get .`. The parentheses are the allocation's **initializer**, which is what
/// `parse_new_initializer` reads once the type stops where it should. A token only an expression has (`.`, `->`,
/// `+`, …) now says so; `-` is deliberately not in that list, because `= -1` is a default argument.
#[test]
fn a_placement_new_initialiser_may_hold_an_expression() {
    assert_reads(
        Where::File,
        &[
            "struct T { T(int); };\nstruct A { int b; int c(); };\nvoid f(void* p, A& a) { ::new (p) T(a.b); }",
            "struct T { T(int); };\nstruct A { int c(); };\nvoid f(void* p, A& a) { ::new (p) T(a.c()); }",
            "struct T { T(int); };\nvoid f(void* p, int x) { ::new (p) T(x + 1); }",
            "struct N { N(int, int); };\nstruct A { int c(); };\nvoid f(A& a) { new N(a.c(), 2); }",
            // The shapes the predicate exists for, which must keep their readings.
            "struct T { T(int); };\nvoid f(void* p) { ::new (p) T(1); }",
            "auto n = sizeof(void(int));",
            "struct Widget { Widget(int); };\nWidget* w = new (Widget)(1);",
            "void g(int);",
        ],
    );

    // **What the type is, and that the parentheses became the initializer.** A `FunctionType` here would be the
    // mis-reading: the type of the allocation is `T`, and `(…)` initialises it.
    let tree = CppParser::parse(
        "struct T { T(int); };\nstruct A { int c(); };\nvoid f(void* p, A& a) { ::new (p) T(a.c()); }",
        ParserConfig::default(),
    );
    assert!(tree.get_errors().is_empty(), "no diagnostics");
    let type_id: Vec<String> = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TypeId)
        .map(|node| node.text().to_string().trim().to_string())
        .collect();
    assert!(
        type_id.iter().any(|text| text == "T"),
        "the allocated type is `T`, not `T(a.c())`: {type_id:?}"
    );
    assert!(
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NewExpr
                && node
                    .descendants()
                    .any(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::Initializer)
        }),
        "the parentheses are the allocation's initializer"
    );
}

/// The empty string when the parse is clean, or a description of the first thing wrong with it.
fn report(source: &str, tree: &CppSyntaxTree) -> Result<(), String> {
    if let Some(error) = tree.get_errors().first() {
        return Err(format!("{} at {:?}", error.message, error.range));
    }

    // A `MissingNode` is a claim that a token should have been there, and an `ErrorNode` is a construct the
    // grammar gave up on. Both are failures for this file's purposes even when nothing was *reported*: a
    // missing declarator name is how `int bits : 3` is read today, and no error message says so.
    for node in tree.get_red_root().descendants() {
        match CppSyntaxKind::from(node.kind()) {
            CppSyntaxKind::ErrorNode => {
                return Err(format!("ErrorNode over {:?}", node.text_range()));
            }
            CppSyntaxKind::MissingNode => {
                return Err(format!("MissingNode at {:?}", node.text_range()));
            }
            _ => {}
        }
    }

    let _ = source;
    Ok(())
}

/// Pin a list of constructs as readable, reporting **every** failure rather than the first.
#[track_caller]
fn assert_reads(place: Where, constructs: &[&str]) {
    let failures: Vec<String> = constructs
        .iter()
        .filter_map(|construct| {
            reads(construct, place)
                .err()
                .map(|why| format!("  {construct}\n      {why}"))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {} constructs no longer parse:{}",
        failures.len(),
        constructs.len(),
        failures
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
    );
}

/// Pin a list of constructs as **not** readable, so that each one is a deliberate entry rather than a
/// surprise. A construct that starts working fails here, which is the signal to move it up.
#[track_caller]
fn assert_does_not_read_yet(place: Where, constructs: &[(&str, &str)]) {
    let now_working: Vec<&str> = constructs
        .iter()
        .filter(|(construct, _)| reads(construct, place).is_ok())
        .map(|(construct, _)| *construct)
        .collect();

    assert!(
        now_working.is_empty(),
        "these constructs parse now, so move them into the list of what is read:\n  {}",
        now_working.join("\n  ")
    );
}

/// A construct and the node kind its **statement** has to come out as.
///
/// The two lists above this one answer "does it parse?", and that question has a blind spot wide enough to hide a
/// whole class of defect. A tree can be well formed, lossless, free of every diagnostic and of every `ErrorNode`,
/// and still describe the wrong construct:
///
/// ```text
/// void f() { x = 1; }
///
/// Syntax(Declaration)              <- an assignment, read as a declaration
///   Syntax(DeclSpecifierSeq)  x
///   Syntax(InitDeclarator)
///     Token(Assign) "="
///     Syntax(Initializer) 1
/// ```
///
/// Nothing above catches that. Well-formedness holds for a wrong tree as much as a right one; `reads` looks for
/// errors and error nodes, and there are none; and the scope layer *already* declines to bind a declarator that
/// named nothing, so the false tree and the true one yield the same (empty) set of names. It was found by hand,
/// while probing something else.
///
/// So this list asks the question the others cannot: **what did it read it as?** Each entry names the construct
/// and the kind of the statement it must produce, and the statement is taken as the child of the enclosing body
/// — a leaf token has no children, so the constructs are exactly the nodes that do.
///
/// A statement kind is not enough on its own for an operator, though: `f(a, b)` and `f((a, b))` are both
/// `ExpressionStat`, and the whole difference between them is *inside*. So an entry may also name an expression
/// node that the construct must contain, and whether it is required or forbidden. That is what pins the comma
/// operator — the defect it could cause is a call with one argument instead of two, which no statement-level
/// assertion can see.
struct Shape {
    construct: &'static str,
    /// File scope input, or a fragment to place inside `void f() { ... }`.
    place: Where,
    /// The kind the statement node must have.
    kind: CppSyntaxKind,
    /// An expression node the construct must contain, or must not contain.
    expression: Option<(CppSyntaxKind, Presence)>,
}

/// Whether the expression named by a [`Shape`] has to be there or has to be absent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// The construct is this — a comma expression, a cast.
    Required,
    /// The construct must **not** be this: the tokens that look like it belong to something else — the comma of
    /// an argument list, the `*` of a multiplication.
    Forbidden,
}

/// The statement a fragment comes out as: the child of the body that holds it, or the root's own child at file
/// scope.
///
/// The statement is picked out by **kind**, not by "has children": a `return;` is two tokens and no child nodes
/// at all, so a filter on having children skips it and answers with the function declaration that encloses it.
fn statement_kind(source: &str) -> Option<CppSyntaxKind> {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();

    /// The kinds a fragment is allowed to be read as. Tokens are `None`, which is what filters the trivia out.
    fn statement_kind_of(kind: CppSyntaxKind) -> Option<CppSyntaxKind> {
        matches!(
            kind,
            CppSyntaxKind::Declaration
                | CppSyntaxKind::ExpressionStat
                | CppSyntaxKind::ReturnStat
                | CppSyntaxKind::IfStat
                | CppSyntaxKind::ForStat
                | CppSyntaxKind::RangeForStat
                | CppSyntaxKind::WhileStat
                | CppSyntaxKind::DoWhileStat
                | CppSyntaxKind::SwitchStat
                | CppSyntaxKind::TryStat
                | CppSyntaxKind::ThrowStat
                | CppSyntaxKind::BreakStat
                | CppSyntaxKind::ContinueStat
                | CppSyntaxKind::CompoundStat
                | CppSyntaxKind::LabelStat
                | CppSyntaxKind::EmptyStat
        )
        .then_some(kind)
    }

    root.descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .and_then(|body| {
            body.children()
                .filter_map(|child| statement_kind_of(CppSyntaxKind::from(child.kind())))
                .last()
        })
        .or_else(|| {
            root.children()
                .next()
                .and_then(|node| statement_kind_of(CppSyntaxKind::from(node.kind())))
        })
}

/// Is there a node of this kind anywhere in the parsed fragment?
fn contains(source: &str, kind: CppSyntaxKind) -> bool {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .any(|node| CppSyntaxKind::from(node.kind()) == kind)
}

/// How many nodes of this kind the parsed fragment has — for the assertions where the *count* is the claim.
fn count_of(source: &str, kind: CppSyntaxKind) -> usize {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// A macro **from a header** can stand where a declaration goes, and only where nothing else can.
///
/// The shape that made this necessary is `namespace std _GLIBCXX_VISIBILITY(default) {`, which opens **every**
/// libstdc++ header, and `_GLIBCXX_BEGIN_NAMESPACE_VERSION` on a line of its own inside the body. Neither name is
/// in any table the parser can be handed — the `#define`s are in `bits/c++config.h`, an *included* file — so the
/// shape is what decides, and it decides only where it has no competitor. Measured on the closure of six standard
/// headers: 48 of 185 files read cleanly before these two rules, 78 after (`docs/std-library.md`).
///
/// The negative assertions are the point of the test. Each is a shape that looks similar and must keep the
/// reading it already had:
///
/// ```text
/// x = 1;                the `=` continues an expression — the first version of the rule read this as a macro
/// FOO(x);               the most vexing parse: a declaration, and the `;` is what says so
/// TEST(A, B) { … }      a definition whose body is the block
/// COUNT (in a body)     a missing `;`, which must stay an error
/// ```
///
/// The first and the fourth are the two mistakes the rule was rewritten to avoid, so they are pinned here rather
/// than left to the rules that own them: widening this one again is exactly what would break them.
#[test]
fn a_macro_from_a_header_can_stand_where_a_declaration_goes() {
    // The namespace head, which is the shape in every standard header.
    let header = "namespace std _GLIBCXX_VISIBILITY(default)\n{\n  struct Widget { int size; };\n}\n";
    assert!(
        contains(header, CppSyntaxKind::NamespaceDecl),
        "the namespace is still a namespace"
    );
    assert!(
        contains(header, CppSyntaxKind::CompoundStat),
        "and it still opens a body — the macro is between the name and the brace, not instead of either"
    );
    assert!(
        CppParser::parse(header, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "and the head no longer costs the whole file"
    );

    // A bare invocation standing for a declaration, with and without arguments.
    let bare = "_GLIBCXX_BEGIN_NAMESPACE_VERSION\ntemplate <typename T> struct S { T x; };\n";
    assert!(contains(bare, CppSyntaxKind::MacroCall));
    assert!(
        CppParser::parse(bare, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "and what follows it is read as the declaration it is"
    );
    assert!(contains(
        "_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)\nint x;\n",
        CppSyntaxKind::MacroCall
    ));

    // …and the wrapper most of libstdc++ is actually written in: a **linkage specification's block**, which is a
    // scope for names but *not* a body. Reading it as a body is what made every macro in there fail, and the
    // symptom was nowhere near the cause — `using ::wint_t;` reported `expected ; after expression`, because the
    // macro above it had been read as a name and the declaration behind it as an expression.
    let in_a_linkage_block = "extern \"C++\"\n{\nnamespace std\n{\n_GLIBCXX_BEGIN_NAMESPACE_VERSION\n\
                              struct Widget { int size; };\n}\n}\n";
    assert!(
        contains(in_a_linkage_block, CppSyntaxKind::MacroCall),
        "the macro is still a macro inside a linkage block"
    );
    assert!(
        CppParser::parse(in_a_linkage_block, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "and the declaration behind it is read as one"
    );

    // The shapes that must keep their reading.
    assert!(
        !contains("x = 1;\n", CppSyntaxKind::MacroCall),
        "an assignment: no declarator follows an `=`, and asking about declarators is what read this as a macro"
    );
    assert!(
        contains("FOO(x);\n", CppSyntaxKind::Declaration),
        "the most vexing parse is a declaration, and the `;` is what says so"
    );
    assert!(
        contains("TEST(A, B) { int x = 1; }\n", CppSyntaxKind::Declaration),
        "a macro used as a definition: the block is the declaration's body"
    );
    assert!(
        !contains("void f() { COUNT\n  return; }\n", CppSyntaxKind::MacroCall),
        "inside a body a missing `;` is the likelier story, and an error is the honest answer"
    );
    assert!(
        !contains("void f() { FOO(x) }\n", CppSyntaxKind::MacroCall),
        "and a call inside a body is a call, however it is spelled"
    );
}

/// **A macro whose body is a brace is read as that construct, whatever follows it** (B120) — and the body may
/// live in an *included* header, which is the whole of MSVC's spelling of `namespace std`.
///
/// The defect this pins was silent, and silence is why it needs a *shape* assertion rather than a diagnostic
/// count: ``_STD_BEGIN struct vector { int size; };`` parses **cleanly** whichever way it is read, so a census of
/// errors cannot tell the two readings apart (`docs/grammar-gaps.md`, the A0 class). What tells them apart is
/// where the `StructDef` ended up — inside the invocation's specifier sequence, or beside it as the declaration
/// it is — and that is what the four cases below assert, one per channel the evidence can come through:
///
/// ```text
/// 1. this file's own `#define`                    ← `bits/c++config.h`, `_GLIBCXX_BEGIN_NAMESPACE_VERSION`
/// 2. a body in force, from an included header     ← MSVC's `_STD_BEGIN`, measured in the closure of <vector>
/// 3. a definition *and* a body, from the caller   ← the flat table `macro_evidence` answers from
/// 4. a follower that cannot begin a declaration   ← where no shape rule can rescue the reading
/// ```
///
/// Case 4 is the one that separates the rule from the coincidence: in 1–3 the token after the invocation
/// begins a declaration (`struct`, `template`), so the *shape* rules read it as a whole statement by accident.
/// `_STD_BEGIN vector<int> x = {};` has no such accident — an identifier follows — and only a rule that asks the
/// body can read it. All **38** `_STD_BEGIN` sites in the closure of `<vector>`, `<string>` and `<map>` happen
/// to be followed by a declaration-starting token, so the corpus cannot tell the two apart either; this test is
/// the only place the difference is measured.
///
/// The last assertion is the negative half, and the one that keeps the rule from being a spelling: with **no**
/// evidence about the name, nothing is claimed — `_STD_BEGIN` stays the ordinary name it is, and the declaration
/// reading is the one a file with no table gets.
#[test]
fn a_macro_whose_body_is_a_brace_is_read_as_that_construct() {
    // The body as the two channels carry it: `namespace std {` and the closer `}`.
    let in_force = || {
        MacroEnvironment::from_included_macros([]).with_bodies_in_force([
            (Box::from("_STD_BEGIN"), Box::from("namespace std {")),
            (Box::from("_STD_END"), Box::from("}")),
        ])
    };
    let described = || {
        MacroEnvironment::from_included_macros([
            IncludedMacro::defined_with_body(
                0,
                "_STD_BEGIN",
                false,
                MacroBody::Unknown,
                Some("namespace std {"),
            ),
            IncludedMacro::defined_with_body(0, "_STD_END", false, MacroBody::Unknown, Some("}")),
        ])
    };

    let body_of_the_file = "#define _STD_BEGIN namespace std {\n#define _STD_END }\n";
    let class_behind_it = "_STD_BEGIN\nstruct vector { int size; };\n_STD_END\n";
    // Case 4's follower: `vector<int> x = {};` — an identifier, so nothing follows that begins a declaration.
    let identifier_behind_it = "_STD_BEGIN\nvector<int> x = {};\n_STD_END\n";

    // (1) The file's own `#define`. No environment at all: the body is in the text being parsed.
    let own = shape_body_of_the_file_reads(&format!("{body_of_the_file}{class_behind_it}"), None);
    assert_eq!(
        own,
        vec![
            "PreprocessorDirective:#",
            "PreprocessorDirective:#",
            "MacroCall:_STD_BEGIN",
            "Declaration:struct",
            "MacroCall:_STD_END",
        ],
        "an invocation whose body opens a namespace is the whole statement, and the class is a declaration \
         beside it rather than the payload of the invocation"
    );

    // (2) The body in force, from a header — the shape MSVC's `<vector>` has.
    let from_a_header = shape_body_of_the_file_reads(class_behind_it, Some(&in_force()));
    assert_eq!(
        from_a_header,
        vec!["MacroCall:_STD_BEGIN", "Declaration:struct", "MacroCall:_STD_END"],
        "and it does not matter that the `#define` is in another file"
    );

    // (3) A definition *and* a body: the name is a macro by the caller's table, which is the evidence the older
    // rules used to read as "this is a declaration's head" — and `name(args) template<…>` is a macro statement,
    // so the declaration behind it must survive.
    let described_and_shaped = shape_body_of_the_file_reads(class_behind_it, Some(&described()));
    assert_eq!(
        described_and_shaped,
        vec!["MacroCall:_STD_BEGIN", "Declaration:struct", "MacroCall:_STD_END"],
        "a name the tables know as a macro is still read from its body"
    );

    // (4) A follower no shape rule can place: `vector<int> x = {};` is a declaration whose first token is an
    // identifier, which is also how a *specifier* continues — so the invocation is claimed by the body alone.
    let no_shape_to_lean_on = shape_body_of_the_file_reads(identifier_behind_it, Some(&in_force()));
    assert_eq!(
        no_shape_to_lean_on,
        vec!["MacroCall:_STD_BEGIN", "Declaration:vector", "MacroCall:_STD_END"],
        "with no declaration-starting token after it, the body is the only thing that can read this"
    );

    // …and the negative half, in the two parts that matter.
    //
    // **No evidence, no claim** — asked of the follower no shape rule can place, because that is the only place
    // where the answer is about *this* rule. With `struct` behind the invocation the *shape* rules claim it
    // anyway (`at_a_macro_that_stands_for_a_declaration`: an unknown name followed by a token that begins a
    // declaration is a macro statement, B87) — which is exactly why the corpus cannot measure this rule: every
    // one of the 38 `_STD_BEGIN` sites has such a follower, so both readings agree there.
    let without_evidence_or_a_shape = shape_body_of_the_file_reads(identifier_behind_it, None);
    assert_eq!(
        without_evidence_or_a_shape.first().map(String::as_str),
        Some("Declaration:_STD_BEGIN"),
        "a name nothing has described is not a licence to open a namespace: {without_evidence_or_a_shape:?}"
    );
    assert!(
        !without_evidence_or_a_shape.contains(&"MacroCall:_STD_BEGIN".to_string()),
        "and the invocation is not claimed by this rule: {without_evidence_or_a_shape:?}"
    );

    // …and B87's *shape* half is untouched, which is what keeps the two rules from being one: the follower that
    // makes the shape rule fire still does, environment or not.
    assert_eq!(
        shape_body_of_the_file_reads(class_behind_it, None),
        vec!["MacroCall:_STD_BEGIN", "Declaration:struct", "MacroCall:_STD_END"],
        "a declaration-starting follower is enough on its own, and that reading must not change"
    );

    // The bodies the rule refuses keep their readings — asked, like the negative above, of the follower no shape
    // rule can place: an empty body (`#define POINTER_32`) and a namespace *name* (`#define _GLIBCXX_MATH_NS __8`).
    // Neither ends at a `{`, so neither opens anything.
    for body in ["", "__8", "namespace std"] {
        let environment = MacroEnvironment::from_included_macros([])
            .with_bodies_in_force([(Box::from("_STD_BEGIN"), Box::from(body))]);
        let shape = shape_body_of_the_file_reads(identifier_behind_it, Some(&environment));
        assert_eq!(
            shape.first().map(String::as_str),
            Some("Declaration:_STD_BEGIN"),
            "a body of {body:?} does not open a namespace: {shape:?}"
        );
    }

    // **`extern "C" {` is an opener, and that answer changed** (B122): this test used to pin it as *refused*,
    // because the rule's vocabulary was "a namespace head" and nothing else. The statement-level openers —
    // `try {`, `do {`, `extern "C" {` — are the same evidence about a different construct: the file writes an
    // invocation where a `{` belongs, and the invocation is a whole statement with the brace inside its body. So
    // the reading is the honest one for this shape, and the list of shapes has a third entry rather than a
    // special case.
    let linkage = MacroEnvironment::from_included_macros([])
        .with_bodies_in_force([(Box::from("_STD_BEGIN"), Box::from("extern \"C\" {"))]);
    assert_eq!(
        shape_body_of_the_file_reads(identifier_behind_it, Some(&linkage)),
        vec!["MacroCall:_STD_BEGIN", "Declaration:vector", "MacroCall:_STD_END"],
        "a linkage block's head is a whole statement, so the declaration after it is read as one"
    );
}

/// The top level of a parse, as `Kind:first-token` — the shape a reading is judged by.
///
/// Losslessness and a clean parse are asserted here rather than at each call site, so that a shape assertion
/// cannot be satisfied by a tree that lost text or reported something on the way.
fn shape_body_of_the_file_reads(source: &str, environment: Option<&MacroEnvironment>) -> Vec<String> {
    let config = match environment {
        Some(environment) => ParserConfig::default().with_macros_from_includes(environment),
        None => ParserConfig::default(),
    };

    let tree = CppParser::parse(source, config);
    assert_eq!(tree.to_source_text(), source, "losslessness");
    assert_eq!(tree.get_errors(), [], "and no diagnostics: {source:?}");

    tree.get_red_root()
        .children()
        .map(|node| {
            format!(
                "{:?}:{}",
                CppSyntaxKind::from(node.kind()),
                node.first_token()
                    .map(|token| token.text().trim().to_string())
                    .unwrap_or_default()
            )
        })
        .collect()
}

/// **A macro whose body ends at a `::` supplies a qualified name's qualifier** (B121) — `_STD`, which MSVC's
/// `yvals_core.h` defines as `::std::`.
///
/// The shape is two names in a row (`_STD addressof(*p)`), which is not an expression in any reading, so the rule
/// has to come from the replacement list: a body that **ends** at a `::` is a nested-name-specifier, and the name
/// after the invocation continues the same qualified name. Both grammars need it — the expression one for
/// `_STD addressof(x)`, the type one for `_STD reverse_iterator<iterator>` — and both are asserted here, because
/// a rule that works in one position says nothing about the other (the same reason the nine seams of a
/// declaration are pinned one at a time).
///
/// The negative is the part that keeps this from being a spelling: with **no** evidence the name is an ordinary
/// name, two names in a row stay the syntax error they look like, and no scope is opened anywhere.
#[test]
fn a_macro_whose_body_ends_at_a_scope_qualifies_the_name() {
    let expression = "void f(int *p) {\n    g(p ? _STD addressof(*p) : nullptr);\n}\n";
    let declared = "struct S {\n    using iterator = int;\n    using reverse = _STD reverse_iterator<iterator>;\n};\n";

    // (1) With the body: the ternary's middle operand is one qualified call, and the invocation is a `MacroCall`
    // — nothing dressed up as a name the file did not write.
    let tree = CppParser::parse(
        expression,
        ParserConfig::default().with_macros_from_includes(&qualifier_body()),
    );
    assert_eq!(tree.get_errors(), [], "one qualified name: no diagnostics");
    assert_eq!(tree.to_source_text(), expression, "losslessness");
    assert!(
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr
                && node.children().any(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::IdentifierExpr
                        && child.children().any(|held| {
                            CppSyntaxKind::from(held.kind()) == CppSyntaxKind::MacroCall
                        })
                })
        }),
        "the invocation is part of the callee's qualified name: {:?}",
        tree.get_errors()
    );

    // (2) The type position, where the same two names have to become one type name rather than a type plus a
    // recovery declaration — which is what made MSVC's `<vector>` file an empty-named fact in `std::vector`.
    let tree = CppParser::parse(
        declared,
        ParserConfig::default().with_macros_from_includes(&qualifier_body()),
    );
    assert_eq!(tree.get_errors(), [], "one qualified type name");
    assert!(
        !tree
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode),
        "and no recovery node: {:?}",
        tree.get_errors()
    );

    // (3) The negative: **no evidence, no claim**. The *shape* is what says so, and an error count would not:
    // inside an argument list the same tokens come out as a recovered `ArgumentList` with no diagnostic at all —
    // which is exactly why maintenance convention 13 says a successful parse is not evidence.
    let without = CppParser::parse(expression, ParserConfig::default());
    assert!(
        !has_a_qualified_call(&without),
        "with nobody saying what `_STD` stands for, two names in a row are not one qualified name: {:?}",
        without.get_errors()
    );
    assert!(
        !without
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "…and nothing claims the name is a macro"
    );
}

/// Is there a call whose callee is a qualified name built from a macro invocation? — the shape the case above
/// asserts, written once so the positive and the negative cannot drift apart.
fn has_a_qualified_call(tree: &CppSyntaxTree) -> bool {
    tree.get_red_root().descendants().any(|node| {
        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr
            && node.children().any(|child| {
                CppSyntaxKind::from(child.kind()) == CppSyntaxKind::IdentifierExpr
                    && child
                        .children()
                        .any(|held| CppSyntaxKind::from(held.kind()) == CppSyntaxKind::MacroCall)
            })
    })
}

/// An environment whose only content is `_STD`'s body, on the **in-force** channel — the one MSVC's conditional
/// definition arrives through.
fn qualifier_body() -> cpp_parser::MacroEnvironment {
    cpp_parser::MacroEnvironment::from_included_macros([])
        .with_bodies_in_force([(Box::from("_STD"), Box::from("::std::"))])
}

/// The compiler's own attribute spellings are attributes, in every position the standard spelling works in.
///
/// `__attribute__((…))` and `__declspec(…)` mean what `[[…]]` means, and they are the *compiler's* extension
/// rather than the file's macro: the standard reserves both names, so matching them by spelling is not the
/// convention-without-evidence that `docs/grammar-gaps.md` entry 16 warns about — there is no `#define` anywhere
/// that could make them something else. libstdc++ writes them in positions a declaration cannot otherwise have
/// anything in: between a template head and the declaration it wraps, between `extern "C++"` and the declaration,
/// and after a parameter list.
///
/// The three positions are pinned separately because they are read by three different call sites — the template
/// head, the specifier sequence and the declarator's suffix — and one of them working says nothing about the
/// others.
#[test]
fn the_compilers_attribute_spellings_are_attributes() {
    let parses = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "this must parse cleanly: {source:?}"
        );
        tree
    };

    // Between the template head and the declaration it wraps — `bits/move.h`.
    let wrapped = parses("template <typename T>\n__attribute__((__always_inline__))\ninline T* addressof(T& r);\n");
    assert!(
        wrapped
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList),
        "and it is an attribute node, not a name the specifier sequence took for a type"
    );

    // In the specifier sequence, after a linkage specification — `bits/c++config.h`.
    parses("extern \"C++\" __attribute__ ((__noreturn__, __always_inline__))\ninline void f() noexcept { }\n");

    // After a parameter list, where the standard spelling already worked.
    parses("void f() __attribute__ ((__noreturn__));\n");

    // The other extension, spelled the other way.
    parses("__declspec(dllexport) void g();\n");

    // And the run: two and three macro names before the declaration they decorate, which is how libstdc++ opens
    // a nested namespace and then a versioned one.
    parses("namespace n { }\n_GLIBCXX_BEGIN_NAMESPACE_VERSION\n_GLIBCXX_BEGIN_NAMESPACE_CONTAINER\ntemplate <typename> struct S;\n");

    // What the run must **not** swallow: a declaration whose type and name are both plain identifiers, and whose
    // declarator happens to carry a parameter list. The first version of the run scanner stepped one token too
    // far past a group and read this as two macro invocations, which left the `;` on the next declaration.
    let declaration = parses("namespace std {\nsize_t\n_Hash_bytes(const void* p);\n}\n");
    assert!(
        !declaration
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "`size_t _Hash_bytes(const void*);` is a declaration with a macro-looking type, not two macros"
    );
}

/// A macro standing among a declarator's **suffixes** is a macro, and the three names that are not stay as they are.
///
/// After a declarator an identifier has exactly two readings in C++ — a contextual keyword or a macro — and
/// everything else that may legally stand there is a keyword or punctuation. So the shape costs no valid program
/// and the alternative is an error; libstdc++ puts one of these after nearly every declaration it writes:
///
/// ```text
/// inline void __terminate() _GLIBCXX_USE_NOEXCEPT
/// T* addressof(T& r) _GLIBCXX_NOEXCEPT { … }
/// bool before(const type_info&) const _GLIBCXX_NOEXCEPT;
/// void f() _GLIBCXX_NOEXCEPT_IF(noexcept(g()));
/// extern "C" void abort(void) _GLIBCXX_NOTHROW _GLIBCXX_NORETURN;      two in a row
/// int x MY_DECL_SUFFIX;                                             after a *variable*'s name
/// ```
///
/// The three refusals are the point of the negative half. `override` and `final` are the contextual keywords that
/// legitimately stand here, and `requires` begins a **clause** the declarator loop reads for itself — taking it
/// for a macro would swallow the constraint and leave its tokens on the declaration that follows, which is what
/// the last assertion checks.
#[test]
fn a_macro_can_stand_among_a_declarators_suffixes() {
    let parses = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "this must parse cleanly: {source:?}");
        tree
    };

    let suffixes = parses(
        "inline void __terminate() _GLIBCXX_USE_NOEXCEPT { }\n\
         T* addressof(T& r) _GLIBCXX_NOEXCEPT { return nullptr; }\n\
         bool before(const type_info& a) const _GLIBCXX_NOEXCEPT;\n\
         void f() _GLIBCXX_NOEXCEPT_IF(noexcept(g()));\n\
         extern \"C\" void abort(void) _GLIBCXX_NOTHROW _GLIBCXX_NORETURN;\n",
    );
    assert_eq!(
        suffixes
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count(),
        6,
        "each one is a macro invocation, including the two in a row"
    );

    // After a **variable**'s name, which is the same position one level down.
    let variable = parses("int x MY_DECL_SUFFIX;\n");
    assert!(
        variable
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "and it is a macro there too"
    );

    // The refusals.
    let keywords = parses("struct S {\n  void f() override;\n  void g() final;\n};\n");
    assert!(
        !keywords
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "`override` and `final` are contextual keywords, not macros"
    );

    let clause = parses("template <typename T>\nvoid f(T) requires C<T>;\n");
    assert!(
        clause
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::RequiresClause),
        "and `requires` still begins a clause: the constraint is read, not swallowed as a macro"
    );
}

/// A **directive inside a declaration** is read where the declaration continues — and the members written after
/// it stay members.
///
/// Seven constructs, six of them from `bits/basic_string.h`, all of them the same seam: a rule is waiting for one
/// particular token and the file writes something else there — a `#` directive, or (the last one) a second
/// specifier name where a declarator was expected. The reading is documented where it happens; what this test
/// pins is that the **declaration around the seam survives it**.
///
/// # Why the count, and not just "it parses"
///
/// [`assert_reads`] cannot see the defect these were. A class member read wrongly does not have to produce an
/// error, an `ErrorNode` or a missing node: what it does is nest every member written after it *inside* the bogus
/// declaration — lossless, well formed, silent, and with all of those members no longer members of the class.
/// That is exactly what `bits/basic_string.h` did: measured before these seven, `std::basic_string` had **117**
/// indexed members and none of the public interface; after them it has **326**, and `examples/std_query.rs` went
/// from 0/7 to 2/7 (`docs/roadmap.md` §2.1). So each entry says how many members the class has, and the
/// assertion is that the number did not change.
#[test]
fn a_directive_inside_a_declaration_keeps_the_members_after_it_members() {
    let shapes: &[(&str, usize)] = &[
        // A constrained constructor whose clause is followed by a directive and then its initializer list
        // (`bits/basic_string.h:585`). The `:` used to be nobody's token, and `_M_dataplus(_M_local_data())` a
        // declaration of its own.
        (
            "S()\n_GLIBCXX_NOEXCEPT_IF(is_nothrow_default_constructible<_Alloc>::value)\n\
             #if __cpp_concepts\nrequires is_default_constructible_v<_Alloc>\n#endif\n\
             : _M_dataplus(_M_local_data())\n{ }\nint after;",
            2,
        ),
        // A member whose **head is conditional** (`bits/basic_string.h:700`): the `#if` half is read where a
        // member goes, and the `#endif` arrives after the head.
        (
            "int a;\n#if __cpp_deduction_guides\ntemplate<typename = _RequireAllocator<_Alloc>>\n#endif\n\
             S(const _CharT* __s);\nint after;",
            3,
        ),
        // …and the same member with a **head per branch** (`bits/basic_string.h:845`): two heads for one
        // declaration, one of them after the `#else`.
        (
            "int a;\n#if __cplusplus >= 201103L\ntemplate<typename _InputIterator,\n\
             typename = std::_RequireInputIter<_InputIterator>>\n#else\ntemplate<typename _InputIterator>\n\
             #endif\nS(const _InputIterator& __beg)\n{ }\nint after;",
            3,
        ),
        // …and with a **specifier between the two heads** (`bits/basic_string.h:1673`), which is the one the
        // specifier sequence has to read for itself.
        (
            "int a;\n#if __cplusplus >= 201103L\ntemplate<class _InputIterator,\n\
             typename = std::_RequireInputIter<_InputIterator>>\n_GLIBCXX20_CONSTEXPR\n\
             #else\ntemplate<class _InputIterator>\n#endif\nS&\nappend(_InputIterator __first)\n\
             { return *this; }\nint after;",
            3,
        ),
        // An attribute followed by the directive that closes its conditional (`bits/basic_string.h:1310`): the
        // attribute is a specifier, so the directive arrives *inside* the sequence.
        (
            "int a;\n#if __cplusplus > 201703L\n[[deprecated(\"use shrink_to_fit() instead\")]]\n#endif\n\
             void reserve();\nint after;",
            3,
        ),
        // Two unexpanded macros and then a keyword type (`bits/basic_string.h:1329`): the second name has to join
        // the type, or `_GLIBCXX20_CONSTEXPR` becomes the declarator and `empty` stops being a member.
        (
            "int a;\n_GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\nbool\nempty() const\n{ return true; }\nint after;",
            3,
        ),
        // An operator-function-name after a type (`bits/basic_string.h:1025`) — the declarator that does not begin
        // with an identifier. Read wrongly, the member was a *variable* called `_If_sv`.
        (
            "int a;\ntemplate<typename _Tp>\n_GLIBCXX20_CONSTEXPR\n_If_sv<_Tp, S&>\n\
             operator=(const _Tp& __svt)\n{ return *this; }\nint after;",
            3,
        ),
        // **A macro suffix after a directive, and one initializer list per branch** — the copy-on-write
        // constructor (`bits/cow_string.h:515`), which is the shape that took 3400 lines of that header with it:
        // the member after it was read at file scope, because the failed constructor's `{ }` was taken for the
        // class's closing brace.
        (
            "S()\n#if _GLIBCXX_FULLY_DYNAMIC_STRING == 0\n_GLIBCXX_NOEXCEPT\n#endif\n\
             #if __cpp_concepts\nrequires is_default_constructible_v<_Alloc>\n#endif\n\
             #if _GLIBCXX_FULLY_DYNAMIC_STRING == 0\n: _M_dataplus(_S_construct(_Alloc()))\n#else\n\
             : _M_dataplus(_S_construct(_Alloc()))\n#endif\n{ }\nint after;",
            2,
        ),
    ];

    for (fragment, members) in shapes {
        let source = format!("struct S {{\n{fragment}\n}};\n");
        assert_eq!(
            reads(&source, Where::File),
            Ok(()),
            "this must parse cleanly: {fragment:?}"
        );
        assert_eq!(
            direct_members(&source),
            *members,
            "every member written after the seam must still be a member: {fragment:?}"
        );
    }
}

/// **A member the parser gives up on does not take the class with it.**
///
/// The recovery's side of the contract above, and the reason the seams are worth anything at all: a declaration
/// that fails **after** consuming tokens leaves the nodes it opened behind, and how they are closed decides whether
/// the rest of the class survives. Closed without their end events — which is what `close_marks_above` does — they
/// are balanced by the tree builder at the **end of the stream**, so the abandoned declaration swallows every
/// member written after it:
///
/// ```cpp
/// struct Base {
///   int x = 1        // no `;` — the declaration gives up *after* reading this much
///   int after;       // …and this became a child of it: still in the tree, no longer a member
/// };
/// ```
///
/// Measured on `bits/stl_vector.h`, one such member (`_GLIBCXX20_CONSTEXPR void f(size_type) { }`) left
/// `std::vector` with **no members at all** — `v.size` and `v.push_back` were both "not declared here", and the
/// class body ran to the end of the file. `parse_declaration` therefore closes them **with** their end events
/// (`MarkerEventContainer::end_marks_to`).
///
/// The assertion asks the question the defect is about — *is the member still a member* — rather than counting
/// nodes, because how many error nodes a given piece of rubble leaves is not a promise worth making.
#[test]
fn a_member_the_parser_gives_up_on_keeps_the_members_after_it_members() {
    let rubble = [
        // The one that cost `std::vector` everything: read as a variable initialised twice, with no `;` to end on.
        "  _GLIBCXX20_CONSTEXPR void f(size_type) { }\n",
        // A plain missing semicolon.
        "  int x = 1\n",
        // A function declarator whose `;` is missing.
        "  void g(int)\n",
        // A macro member nothing can read: `bits/stl_vector.h:464`, the macro defined in an included file.
        "  __glibcxx_class_requires(_Tp, _SGIAssignableConcept)\n",
    ];

    for text in rubble {
        let source = format!("struct Base {{\n{text}  int after;\n}};\n");
        assert!(
            has_a_member_containing(&source, "int after;"),
            "the member after the rubble must still be a member of the class: {text:?}"
        );
    }
}

/// The number of **direct** members a class body holds: its own `Declaration` children, not the declarations
/// nested inside them — which is the whole difference the two tests below are about.
fn direct_members(source: &str) -> usize {
    let Some(body) = class_body(source) else {
        return 0;
    };

    body.children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declaration)
        .count()
}

/// Is there a **direct member** of the class body whose text contains `needle`?
///
/// The assertion for the recovery: the member written after a broken one must still be a member, and how many
/// error nodes the broken one left behind is not a question worth pinning.
fn has_a_member_containing(source: &str, needle: &str) -> bool {
    let Some(body) = class_body(source) else {
        return false;
    };

    body.children()
        .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declaration)
        .any(|child| child.text().to_string().contains(needle))
}

/// The first `ClassBody` of a parsed fragment, if it has one.
fn class_body(source: &str) -> Option<cpp_parser::CppSyntaxNode> {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ClassBody)
}

/// **A declarator takes one initializer** — and the member is still read as what it is.
///
/// The shape is `bits/stl_vector.h:192`, and the reading that made it famous is worth stating precisely, because
/// neither token is unusual on its own:
///
/// ```cpp
/// struct _Grow {
///   _GLIBCXX20_CONSTEXPR void _M_grew(size_type) { }
/// };
/// ```
///
/// A macro this file does not define stands where a return type goes. The suffix reader then decides what
/// `(size_type)` is — a parameter list, or a direct-initialisation of a variable named `_M_grew` — and with a
/// *name* recorded in type position it chose the initializer. `void` is a keyword type in front of that name, which
/// is the one piece of evidence the choice ignored, so the member came out as a variable of type
/// `_GLIBCXX20_CONSTEXPR void` initialised with `size_type` — and then the body's `{ }` became a **second**
/// initializer, which no declaration has. With no `;` to end on, the declaration swallowed the rest of the class:
/// `std::vector` was left with **not one member**.
///
/// Refusing the second initializer is what turns that into an ordinary failure, and the recovery then reads the
/// member correctly with the macro out of the way: one error node for `_GLIBCXX20_CONSTEXPR`, and
/// `void f(size_type) { }` as the function it is. Both halves are asserted — the failure alone would be worthless
/// if the members after it were lost, which is what the test below is for.
#[test]
fn a_declarator_takes_only_one_initializer() {
    let source = "struct S {\n  int a;\n  _GLIBCXX20_CONSTEXPR void f(size_type) { }\n  int after;\n};\n";

    assert_eq!(
        direct_members(source),
        3,
        "the macro becomes an error node, and the two real members are still members"
    );
    assert!(
        has_a_member_containing(source, "void f(size_type) { }"),
        "and the member the macro stood in front of is read as the function it is"
    );
    assert!(
        has_a_member_containing(source, "int after;"),
        "with the member written after it untouched"
    );
}

/// **One construct per branch**: a declaration whose own parts are written twice, once on each side of a
/// conditional.
///
/// Not a seam at one joint but a shape that repeats — and the reason a rule that reads "the token after the `=`"
/// is not enough on its own. Three real ones, each from a different header:
///
/// ```cpp
/// template<typename _Tp, _Tp _Num> using make_integer_sequence   // bits/utility.h:174
/// #if __has_builtin(__make_integer_seq)
///       = __make_integer_seq<integer_sequence, _Tp, _Num>;       // a definition per branch
/// #else
///       = integer_sequence<_Tp, __integer_pack(_Num)...>;
/// #endif
///
/// template<typename _Tp> concept __is_signed_int128              // bits/iterator_concepts.h:615
/// #if __SIZEOF_INT128__
///       = same_as<_Tp, __int128>;
/// #else
///       = false;
/// #endif
///
/// template<typename _Tp, typename _Up>                           // bits/alloc_traits.h:72
/// #if __cpp_concepts
///   requires requires { typename _Tp::template rebind<_Up>::other; }  // a clause the directive pushed away
///   struct __rebind<_Tp, _Up>                                         // …and a head per branch,
/// #else
///   struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>
/// #endif
///   { using type = typename _Tp::template rebind<_Up>::other; };      // …with one body shared by both
/// ```
///
/// The third is the one whose failure was loudest: the first head saw the other branch's `{`, took it for its own
/// body, and the declaration came apart — `bits/alloc_traits.h` lost `__allocator_traits_base` and every
/// declaration after it. So each case asserts both that the file parses **and** that the construct's own node is
/// there, which is what "one declaration" means here.
#[test]
fn a_declaration_written_once_per_branch_is_read_as_one_declaration() {
    let shapes: &[(&str, CppSyntaxKind)] = &[
        (
            "template<typename _Tp, _Tp _Num>\n  using make_integer_sequence\n#if B\n    \
             = __make_integer_seq<integer_sequence, _Tp, _Num>;\n#else\n    \
             = integer_sequence<_Tp, __integer_pack(_Num)...>;\n#endif\n",
            CppSyntaxKind::UsingDecl,
        ),
        (
            "template<typename _Tp>\n  concept __is_signed_int128\n#if B\n\t= same_as<_Tp, __int128>;\n#else\n\t\
             = false;\n#endif\n",
            CppSyntaxKind::ConceptDecl,
        ),
        (
            "template<typename _Tp, typename _Up>\n#if C\n  \
             requires requires { typename _Tp::template rebind<_Up>::other; }\n  \
             struct __rebind<_Tp, _Up>\n#else\n  \
             struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>\n#endif\n  \
             { using type = typename _Tp::template rebind<_Up>::other; };\n",
            CppSyntaxKind::ClassBody,
        ),
    ];

    for (source, kind) in shapes {
        assert_eq!(
            reads(source, Where::File),
            Ok(()),
            "must parse cleanly: {source:?}"
        );
        assert!(
            contains(source, *kind),
            "and the construct is one node, not rubble: {source:?}"
        );
    }
}

/// **A value where a type could stand**: the template-argument and cast spellings that one token decides.
///
/// A template argument is read as a type if it can be, and as an expression otherwise — and the two readings share
/// their tokens far more often than the rule suggests:
///
/// ```text
/// std::function<void()>            a function type with no parameters: the `(` follows `void`
/// BoolConstant<_S_use_relocate()>  a call: the same `(` follows a *name*
/// iter_value_t<_Tp>(iter_move(x))  a call whose payload is a call
/// typename Alloc::is_always_equal{}   a conversion whose type needs the keyword
/// ```
///
/// The first two are the same four tokens with one word changed, so what tells them apart is the word:
/// [`a_parameter_list_is_the_type`] admits an empty group only after a keyword type, and the argument reader falls
/// back to the expression when the type reading stops at a group that cannot be a parameter list. The measured
/// cases are `bits/stl_vector.h` (`__bool_constant<_S_use_relocate()>` — with the type reading winning, the whole
/// `_S_use_relocate` overload set was rubble) and `std::function<void()>`, which had been unreadable everywhere.
#[test]
fn a_template_argument_may_be_a_call_or_a_function_type() {
    assert_reads(
        Where::File,
        &[
            "using F = std::function<void()>;",
            "using G = std::vector<std::function<void()>>;",
            "using H = Fn<void(), int>;",
            "using C = BoolConstant<_S_use_relocate()>;",
            "using D = Other<iter_value_t<_Tp>(iter_move(x)), sizeof(int)>;",
            "using E = T<f()>;",
            "using I = T<f(1, 2), g(x)>;",
            "using J = T<void(int)>;",
            // A functional conversion whose *type* is written with `typename`, which is how a dependent type is
            // named in an expression — `bits/basic_string.h:3944` writes one inside a condition.
            "void f() { if (typename Alloc::is_always_equal{}) g(); }",
            "void f() { x = typename A::x{}; }",
        ],
    );
}

/// **A statement the parser gives up on does not take its block with it** — the recovery contract, at the
/// statement level.
///
/// The same defect as [`a_member_the_parser_gives_up_on_keeps_the_members_after_it_members`] one level down, and
/// the one that cost `std::map` its `find`:
///
/// ```cpp
/// mapped_type& operator[](const key_type& __k) {          // bits/stl_map.h:527
///   __glibcxx_function_requires(_DefaultConstructibleConcept<mapped_type>)   // a macro with no `;`
///
///   iterator __i = lower_bound(__k);
///   …
/// }
/// mapped_type& at(const key_type& __k) { … }              // ← this stopped being a member
/// ```
///
/// The expression statement failed on the missing `;` and **detached** the node it had opened instead of closing
/// it, so the leftover `NodeStart` swallowed the rest of the body — the `}` that ends it included — and then the
/// rest of the class (`std::map`'s member list stopped at the next declaration and `m.find` answered "not
/// declared in this file"). `parse_expression_statement` and `CppParser::recover_to_level` close their nodes
/// **with** their end events.
///
/// The assertion is the member written after the body: it has to still be a member of the class.
///
/// # The other half: a brace the failed statement never closed
///
/// Closing markers with their end events decides what the *tree* looks like; it cannot un-consume a **token**. A
/// statement that failed after eating a `{` — a braced initialiser, a lambda's body, a nested block — therefore
/// left the block one brace short, the recovery skipped *to* the next `}`, and that one belonged to the failed
/// statement: the block ended there and the statements after it were left to the enclosing rule. The fix is a
/// **brace debt** (kept by `parse_stats`, and by the class body for the member-level case — `docs/grammar-gaps.md`
/// B58): the failed statement is charged for the braces it consumed and never closed, and the block pays the debt
/// by reading the next `}`s as error nodes before it may end.
#[test]
fn a_statement_the_parser_gives_up_on_keeps_the_block_after_it() {
    let rubble = [
        // A macro invocation written without its `;` — how libstdc++ writes its concept checks.
        "  __glibcxx_function_requires(_DefaultConstructibleConcept<T>)\n  iterator i = lower_bound(k);\n",
        // A plain missing semicolon.
        "  int x = 1\n  int y = 2;\n",
        // A call that is not a statement.
        "  g(1, 2) h();\n",
        // **A brace the failed statement opened and never closed** — a braced initialiser whose initialiser is
        // written in two branches, so the `;` is in the other one and the statement fails with `{` consumed.
        "  S s{\n#if X\n    1\n#endif\n    ;\n  };\n",
        // …and the same one level in: a lambda's body split by a directive.
        "  auto l = [] {\n#if X\n    g()\n#endif\n    ;\n  };\n",
        // …and a nested block that never closes.
        "  {\n#if X\n    g()\n#endif\n    ;\n",
    ];

    for text in rubble {
        let source = format!("struct Base {{\n  void f() {{\n{text}  }}\n  int after;\n}};\n");
        assert!(
            has_a_member_containing(&source, "int after;"),
            "the member after the block must still be a member of the class: {text:?}"
        );
    }
}

/// A **member access whose name is not written yet** — `w.` — is the state a completion is asked in, and it must
/// not cost the block its `}`.
///
/// The shape of `w.` is fine and has been since the completion entry point was built: it reads as
/// `IndexExpr[IdentifierExpr(w) Dot]`, so the object and the operator are there for a cursor to be resolved
/// against. What was wrong was the **radius of the recovery**: the member name is mandatory in the suffix loop, so
/// the only way out was an `Err`, and an `Err` out of `parse_expr` reaches the statement layer as a failed
/// statement.
///
/// ```text
/// void f() { Widget w; w.  }        an `Err` here                       a `MissingNode` here
///   CompoundStat@9..54   ← closes at 54, not 29                          CompoundStat@9..29
///     ExpressionStat@25..30  ← the IndexExpr ate the `}`                   ExpressionStat@25..28
///     Declaration@30..41     ← `int after;` became a LOCAL of f()          Declaration@30..41  ← file scope
/// ```
///
/// The second shape is what `parse_compound_stat` already does for a missing `}` — `emit_missing_node` plus a
/// reported error, so completion has a node to live in and the user is still told — and the fix is to do the same
/// thing in the suffix loop rather than let `Err` cross the statement boundary. See maintenance convention 20.
#[test]
fn a_member_access_without_a_name_keeps_the_block_after_it() {
    let source = "void f() {\n  Widget w;\n  w.\n}\nint after;\nvoid g() { }\n";
    let tree = CppParser::parse(source, ParserConfig::default());

    // The error is still reported: the absence is a fact about the text, and an editor has to be able to say so.
    assert!(
        tree.get_errors()
            .iter()
            .any(|error| error.message.contains("member access")),
        "the missing member name is still reported: {:?}",
        tree.get_errors()
    );

    // …and the block ends where its `}` is, so `int after;` is at file scope rather than inside `f`.
    assert!(
        tree.get_red_root()
            .descendants()
            .any(|node| {
                CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration
                    && node.text().to_string().starts_with("int after;")
            }),
        "`int after;` is still a declaration: {}",
        tree.to_source_text()
    );

    let local = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .any(|block| block.text().to_string().contains("int after;"));

    assert!(
        !local,
        "and it is not inside the function body — the block kept its own `}}`: {}",
        tree.to_source_text()
    );
}

/// A **speculative read that is rolled back must take its diagnostics with it**.
///
/// C++'s declaration/expression ambiguity is resolved by reading one way, and rewinding if it does not fit — the
/// template-argument case is the common one: `a.b < c > d` is a comparison, `a.b<c> d` is a declaration, and the
/// only way to know is to try. A failed attempt is not a fact about the file, so an error it reported belongs to a
/// reading nobody kept: leaving it behind reports a problem that the accepted reading does not have.
///
/// The file below is valid C++ and reads cleanly; before the fix it carried the diagnostics of the attempt that
/// lost. That is worse than a missing diagnostic, because it tells the user about code that is not there.
#[test]
fn a_rolled_back_reading_takes_its_diagnostics_with_it() {
    let valid = [
        // A comparison written after a member access: the template-argument reading is tried and rewound.
        "void f() { a.b < c > d; }\n",
        // The same with a template member and real arguments, where the reading *is* kept.
        "void f() { a.template b<int>(1); }\n",
        // A pointer-to-member and a cast, whose readings are tried in order.
        "void f() { (Widget*)p; }\n",
        "struct S { int m; };\nvoid f() { int S::*p = &S::m; }\n",
    ];

    for source in valid {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "{source:?} is valid and must read without diagnostics, got {:?}",
            tree.get_errors()
                .iter()
                .map(|error| error.message.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// **Valid C++ written in the shapes a recovery eats.** Every entry here is a construct the parser already read
/// before this round; none of them is new grammar. What they have in common is that each one goes through a
/// ``try one reading, rewind, try another`` decision — a cast against a parenthesised expression, a template
/// argument list against a comparison, a declaration against a statement — and that is where recovery does its
/// damage when it is wrong.
///
/// The list exists because the census cannot see this class of bug. Every construct here is *valid*, so a
/// regression shows up as a diagnostic on code that is not wrong: the file count of the corpus would not move, and
/// an editor would underline a line the user typed correctly. It is the second number of maintenance convention
/// 29, and the three shapes that were tried and rewound on the way are listed with what they must keep.
#[test]
fn valid_cpp_in_the_shapes_recovery_eats() {
    assert_reads(
        Where::Body,
        &[
            // Casts and parenthesised expressions: the same prefix, two readings.
            "(Widget*)p;",
            "(void)p;",
            "int x = (int)y;",
            "g((int)x);",
            "(a)(b)(c);",
            "p = (T*)q;",
            // A template-id against a comparison — the ambiguity the parser exists to solve by trying.
            "A<B> c;",
            "a < b > c;",
            "a.b < c > d;",
            "x = y < z > w;",
            "g<int, double>(x);",
            "T::template f<int>();",
            "typename T::type y;",
            // Initialisation: braces, parens and the two together.
            "X x = {1, 2};",
            "int a[] = {1, 2};",
            "T t{};",
            "A a = A();",
            "new (p) T(1);",
            "decltype(x) y;",
            "sizeof(T);",
            "alignas(16) int x;",
            "static_assert(sizeof(T) > 0);",
            // Bodies inside speculative regions: a lambda is parsed while the enclosing construct is still a guess.
            "auto l = [](int a) { return a; };",
            "g([](){ });",
            // Statements whose head is a declaration.
            "for (auto& x : v) { }",
            "while (a) { }",
            "do { } while (a);",
            "switch (a) { case 1: break; }",
            "goto end; end: ;",
            "if (a) { }",
            "throw X();",
            // Declarations inside a body, including the ones with several declarators and initialisers.
            "int x = 1; int y = 2;",
            "struct S { int x; } s;",
            "enum E { A };",
            // Pointers to members, which the expression grammar reads with the same suffix loop as `.*`.
            "a->*b;",
            "a.*b;",
            "int (S::*p) = &S::m;",
        ],
    );

    assert_reads(
        Where::File,
        &[
            "using T = int;",
            "template<class T> void f(T t) { g<T>(t); }",
            "auto f() -> int { return 0; }",
            "void f(int a, int b) { }",
            "void g() { h(); } void f() { g(); }",
            "void f() { (Widget*)p; }",
            "struct S { S() : m(1) { } int m; };",
            "class C { public: C() = default; ~C(); };",
            "struct S { int x : 3; };",
            "template<class T> concept C = requires (T t) { t.f(); };",
        ],
    );
}

/// **A linkage block written the way every C header writes it** — with the conditional *inside* the braces.
///
/// This is the shape the whole of `winnt.h` hinges on, and it is the canonical C-header idiom:
///
/// ```cpp
/// #ifdef __cplusplus
/// extern "C" {
/// #endif
/// …
/// #ifdef __cplusplus
/// }
/// #endif
/// ```
///
/// The block's loop called `parse_declaration` and nothing else, so the `#endif` was one token wrapped in an
/// `ErrorNode`, `endif` became the next declaration's **type name**, and the `}` that closes the linkage was
/// consumed by whatever came after — the loop then ran to the end of the file. On `winnt.h` that was one
/// `CompoundStat` over 387 000 bytes with **eight** `#endif`s inside it, which is what made the file's conditional
/// nesting unusable for the macro layer. The assertion is the directive count inside the block and the declaration
/// written after it.
#[test]
fn a_linkage_block_keeps_the_directives_written_inside_it() {
    let source = "\
#ifdef __cplusplus
extern \"C\" {
#endif
int x;
#ifdef __cplusplus
}
#endif
int after;
";

    let tree = CppParser::parse(source, ParserConfig::default());
    assert!(
        tree.get_errors().is_empty(),
        "the shape is valid C++ and must read clean: {:?}",
        tree.get_errors()
    );

    let block = tree
        .get_red_root()
        .descendants()
        .find(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat
                && node.text().to_string().starts_with('{')
        })
        .expect("the linkage block is a node");

    let directives = block
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::PreprocessorDirective)
        .count();
    assert_eq!(
        directives, 2,
        "both directives written inside the block are directives, not error nodes: {}",
        block.text()
    );

    assert!(
        !block.descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode
        }),
        "and nothing in it is rubble: {}",
        block.text()
    );

    assert!(
        !block.text().to_string().contains("int after;"),
        "and the block ends at its own `}}`: {}",
        block.text()
    );

    // **A declaration inside the block that fails with a brace left open** must not cost the block either — the
    // third container that keeps a brace debt (`docs/grammar-gaps.md` B58), and the one the MinGW headers needed:
    // a braced initialiser whose initialiser is written in two branches fails with the `{` consumed, and without
    // the debt the block's own `}` paid for it.
    let source = "extern \"C\" {\n  S s{\n#if X\n    1\n#endif\n    ;\n  };\n  int after;\n}\n";
    let block = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .find(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat
                && node.text().to_string().starts_with('{')
        })
        .expect("the linkage block is a node");
    assert!(
        block.text().to_string().contains("int after;"),
        "the declaration after a broken one is still inside the linkage block: {}",
        block.text()
    );
}

/// **A `typedef` declares as many names as it lists** — `typedef WCHAR *PWCHAR, *LPWCH, *PWCH;`.
///
/// The rule read one declarator and insisted on `;`, so every multi-declarator typedef in every C header failed at
/// its first comma. `winnt.h` writes this shape hundreds of times; its 417 errors started here, and through them
/// the file lost its directive structure.
///
/// The assertion is the *second* name being usable as a type: the parser declares each declarator's name so that
/// `LPWCH q;` later reads as a declaration rather than as an expression.
#[test]
fn a_typedef_declares_every_name_it_lists() {
    let source = "\
typedef WCHAR *PWCHAR, *LPWCH;
PWCHAR p;
LPWCH q;
";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert!(
        tree.get_errors().is_empty(),
        "a typedef with two declarators is valid C++: {:?}",
        tree.get_errors()
    );

    let declaration_of = |name: &str| {
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration
                && node.text().to_string().starts_with(name)
        })
    };

    assert!(
        declaration_of("PWCHAR p;"),
        "the first name is a type: {}",
        tree.to_source_text()
    );
    assert!(
        declaration_of("LPWCH q;"),
        "**and so is the second** — the whole point of the list: {}",
        tree.to_source_text()
    );
}

/// **A macro between a class-key and its tag** — `typedef struct DECLSPEC_ALIGN (8) _NAME { … } NAME;`.
///
/// The position a compiler's alignment attribute is written in. The macro lives in `_mingw.h`, another file, so no
/// table can know it and no spelling is evidence — what makes accepting it free is that nothing else can stand
/// there: after a class-key the grammar allows an attribute, a name, `{`, `:` or `;`, and *a name followed by a
/// parenthesised group* is none of them. The guard is what follows it, so a mistake is still a mistake.
#[test]
fn a_class_head_may_carry_a_macro_before_its_name() {
    let source = "\
typedef struct DECLSPEC_ALIGN (8) _XSAVE_AREA_HEADER {
  DWORD64 Mask;
} XSAVE_AREA_HEADER, *PXSAVE_AREA_HEADER;
XSAVE_AREA_HEADER h;
";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert!(
        tree.get_errors().is_empty(),
        "the shape is how mingw-w64 writes every aligned structure: {:?}",
        tree.get_errors()
    );

    assert!(
        tree.get_red_root().descendants().any(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration
                && node.text().to_string().starts_with("XSAVE_AREA_HEADER h;")
        }),
        "and the tag is a type name the file can use: {}",
        tree.to_source_text()
    );
}

/// An **operator name** is a name in an expression too, in all three positions it can be written.
/// `operator<=>(a, b)` is a call to the operator function, and the standard library asks exactly that question
/// when it wants to know whether a type has a comparison: `{ operator<=>(x, y); }` inside a requires-expression
/// is how `std::three_way_comparable` is written. The three positions are separate arms of the expression
/// grammar, so one working says nothing about the others:
///
/// ```text
/// operator<(a, b)              a bare operator name as the callee       bits/ranges_cmp.h
/// x.operator<(y)               an explicit member operator call          the same file, one line down
/// p->~T()   x.~basic_string()  a pseudo-destructor call                  concepts, bits/stl_construct.h
/// ```
///
/// The bare form was the one missing: the name arm of the primary rule listed `Identifier` and `Scope` but not
/// `OperatorKeyword`, so `operator<` was a name *after* a `::` and never one at the start. In a function body it
/// happened to work, because there the **declaration** rule reads `operator<(a, b);` as a conversion-operator
/// declaration — which is why the missing arm went unnoticed: the shape is only reachable as an expression
/// inside a requirement.
#[test]
fn an_operator_name_is_a_name_in_an_expression_too() {
    let parses = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "this must parse cleanly: {source:?}");
        tree
    };

    // The bare callee, in the position that has no declaration reading: a requirement's braced expression.
    parses("template <typename T>\nconcept C = requires(T a) { operator<(a, a); };\n");
    parses("template <typename T, typename U>\nconcept D = requires(T t, U u) { operator<=>(t, u); };\n");

    // The member call and the pseudo-destructor, which are the same question one level down.
    parses("template <typename T, typename U>\nconcept E = requires(T t, U u) { t.operator<(u); };\n");
    parses("template <typename T>\nvoid f(T* p) { p->~T(); }\n");
    parses("struct S { ~S(); };\nvoid g(S s) { s.~S(); }\n");
    parses("void h() { operator~(); operator new(1); }\n");

    // And the call reads the operator **as an operator**: `a.operator<(b)` is not a member called `operator`.
    let member = parses("void f(A a, B b) { a.operator<(b); }\n");
    assert!(
        member
            .get_red_root()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::OperatorKeyword),
        "the operator name is in the tree, read by the same rule a declaration's operator name goes through"
    );
}

/// A construct whose statement kind is all that is asserted.
fn shape(construct: &'static str, place: Where, kind: CppSyntaxKind) -> Shape {
    Shape {
        construct,
        place,
        kind,
        expression: None,
    }
}

/// A construct that must come out as `kind` **and** contain an expression of `expression`.
fn requiring(shape: Shape, expression: CppSyntaxKind) -> Shape {
    Shape {
        expression: Some((expression, Presence::Required)),
        ..shape
    }
}

/// A construct that must come out as `kind` and must **not** contain an expression of `expression` — for the
/// tokens that look like an operator but belong to the construct around them.
fn forbidding(shape: Shape, expression: CppSyntaxKind) -> Shape {
    Shape {
        expression: Some((expression, Presence::Forbidden)),
        ..shape
    }
}

#[track_caller]
fn assert_statement_kind(shapes: &[Shape]) {
    let wrong: Vec<String> = shapes
        .iter()
        .filter_map(|shape| {
            let source = match shape.place {
                Where::File => shape.construct.to_string(),
                Where::Body => format!("void probe() {{ {} }}", shape.construct),
                Where::Class => format!("struct Probe {{ {} }};", shape.construct),
            };

            match statement_kind(&source) {
                Some(kind) if kind != shape.kind => Some(format!(
                    "  {}\n      read as {kind:?}, must be {:?}",
                    shape.construct, shape.kind
                )),
                None => Some(format!(
                    "  {}\n      no statement node at all",
                    shape.construct
                )),
                Some(_) => match shape.expression {
                    None => None,
                    Some((want, Presence::Required)) if contains(&source, want) => None,
                    Some((want, Presence::Required)) => {
                        Some(format!("  {}\n      no {want:?} in it", shape.construct))
                    }
                    Some((unwanted, Presence::Forbidden)) if contains(&source, unwanted) => {
                        Some(format!(
                            "  {}\n      contains {unwanted:?}, which belongs to something else",
                            shape.construct
                        ))
                    }
                    Some(_) => None,
                },
            }
        })
        .collect();

    assert!(
        wrong.is_empty(),
        "{} of {} constructs were read as the wrong node:{}",
        wrong.len(),
        shapes.len(),
        wrong
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
    );
}

#[test]
fn constructs_the_parser_reads() {
    // Declarations, including the ones this file's own history made possible.
    assert_reads(
        Where::File,
        &[
            // Direct-initialisation with a type the file never declares: the point of the whole exercise.
            "Widget w(1, 2, 3);",
            "Widget w(1);",
            "std::string s(\"hi\");",
            "std::vector<std::vector<int>> vv;",
            "struct P { int x; } origin;",
            "struct F; struct G { struct H { int y; }; };",
            // Function and object types written with parenthesised declarators.
            "int (*signal(int sig))(int);",
            "using Callback = void (*)(int);",
            "typedef void (*Old)(int);",
            // Linkage.
            "extern \"C\" void f();",
            "extern \"C\" { void f(); }",
            // `bool` is a type keyword, and it used to be lexed as an identifier.
            "bool g = true;",
            // Contextual and modern specifiers.
            "struct S final : B { };",
            "struct S { explicit operator bool() const; };",
            "struct S { bool operator==(const S&) const = default; };",
            "struct S { S& operator=(const S&) = delete; };",
            // Conversion operators: the name is the type it converts to, and it begins the declaration, so no
            // specifier sequence runs before it.
            "struct S { operator int(); };",
            "struct S { operator bool() const; };",
            "struct S { operator const char*(); };",
            "struct S { operator std::string() const; };",
            "struct S { operator std::vector<int>(); };",
            "struct S { operator T&(); };",
            "struct S { operator T&&(); };",
            "struct S { operator unsigned long long(); };",
            "struct S { operator int() { return 1; } };",
            "struct S { operator int() = delete; };",
            "struct S { operator int() const &; };",
            "struct S { operator bool() &&; };",
            "struct S { virtual operator bool() const noexcept; };",
            // Aliases to array and function types: the type's suffixes have nothing enclosing them here.
            "using Arr = int[4];",
            "using Fn = int(char);",
            "using Ptr = int(*)[4];",
            // Attributes, in the positions that used to be gaps.
            "[[nodiscard]] int attributed();",
            "int attributed2 [[gnu::aligned(16)]];",
            "int attributed3() [[carries_dependency]];",
            "void attributed4() [[noreturn]] { for (;;) {} }",
            "void attributed5(int x [[maybe_unused]]);",
            "void attributed6([[maybe_unused]] int x);",
            "using Alias [[deprecated]] = int;",
            "template <typename T> [[nodiscard]] T attributed7();",
            "enum class Attributed8 { A [[deprecated]] = 1, B };",
            // `alignas`, which is a specifier whose payload is a constant-expression *or* a type-id.
            "alignas(16) struct Aligned { int x; };",
            "alignas(16) int aligned_global;",
            "alignas(int) int aligned_to_int;",
            "alignas(16) alignas(32) int doubly_aligned;",
            "struct Aligned2 { alignas(16) int member; };",
            "alignas(16) Aligned2 aligned_object;",
            "void aligned_param(alignas(16) int x);",
            "alignas(16) char aligned_buffer[64];",
            "enum class E : unsigned char { A, B };",
            "static_assert(sizeof(int) == 4, \"int\");",
            "[[nodiscard]] int f();",
            "using namespace std;",
            "namespace fs = std::filesystem;",
            "namespace A::B { int x; }",
            "auto f() -> int { return 1; }",
            "int f() noexcept { return 1; }",
            "constexpr int k = 1;",
            "inline constexpr int k = 1;",
            "thread_local int t = 0;",
        ],
    );

    // Template parameter packs, in all three spellings.
    assert_reads(
        Where::File,
        &[
            "template <typename... Ts> struct S { };",
            "template <class... Ts> struct S { };",
            "template <typename T, typename... R> void f(T t, R... r);",
            "template <typename... A> void f(A&&... a);",
            "template <typename T = int> struct R { };",
            "template <template <class> class C> struct Q { };",
            // An **alias template**, which is a `using`-declaration with a template head in front of it.
            "template <typename U> using rebind = Rebind<U>;",
            "template <typename... Us> using Pack = std::tuple<int, char>;",
            // A pack expansion as a template argument, in a declaration's type rather than in an expression.
            "template <typename... Ts> using All = std::tuple<Ts...>;",
            // A **macro invocation used where a definition goes** — how gtest, Catch2 and every benchmark library
            // write a test. File scope (or a namespace) is what makes it that shape: inside a function body a call
            // followed by a block is a real error, and the rule refuses it there — so this list is the only one it
            // can be read from. The arguments are the macro's tokens, neither values nor declarators, which is why
            // the shape is the whole of the test.
            "TEST(A, B) { }",
            "TEST(FormatPerformance, 1k_row) { }",
            "TEST(A, B) { int x = 1; }",
            "namespace n { TEST(A, B) { } }",
            // A **macro from a header** in declaration position, which is the shape every libstdc++ header opens
            // with: the name is `#define`d in an *included* file, so no table the parser can be handed knows it and
            // the shape is what decides. File scope is where that works — see
            // `a_macro_from_a_header_can_stand_where_a_declaration_goes` for the shapes that must *not* be read
            // this way, and `docs/grammar-gaps.md` entry 23.
            "namespace std _GLIBCXX_VISIBILITY(default) { int x; }",
            "_GLIBCXX_BEGIN_NAMESPACE_VERSION\nint x;\n",
            "_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)\nint x;\n",
            // A **run** of them, which is how libstdc++ opens a versioned namespace inside a container one.
            "_GLIBCXX_BEGIN_NAMESPACE_VERSION\n_GLIBCXX_BEGIN_NAMESPACE_CONTAINER\ntemplate <typename> struct S;\n",
            // The compiler's own attribute spellings, in the three positions the standard one works in. See
            // `the_compilers_attribute_spellings_are_attributes`.
            "template <typename T>\n__attribute__((__always_inline__))\ninline T* addressof(T& r);\n",
            "extern \"C++\" __attribute__ ((__noreturn__, __always_inline__))\ninline void f() noexcept { }\n",
            "void f() __attribute__ ((__noreturn__));\n",
            "__declspec(dllexport) void g();\n",
            // A **macro among a declarator's suffixes**, which is where libstdc++ puts one — after the parameter
            // list, after a `const`, twice in a row, and after a variable's name. See
            // `a_macro_can_stand_among_a_declarators_suffixes` for the three names that must stay keywords.
            "inline void f() _GLIBCXX_USE_NOEXCEPT { }\n",
            "T* addressof(T& r) _GLIBCXX_NOEXCEPT { return nullptr; }\n",
            "bool before(const type_info& a) const _GLIBCXX_NOEXCEPT;\n",
            "void f() _GLIBCXX_NOEXCEPT_IF(noexcept(g()));\n",
            "extern \"C\" void abort(void) _GLIBCXX_NOTHROW _GLIBCXX_NORETURN;\n",
            "int x MY_DECL_SUFFIX;\n",
            // An **operator name** used as an expression — the question a requires-expression asks about a type.
            // See `an_operator_name_is_a_name_in_an_expression_too`.
            "template <typename T>\nconcept C = requires(T a) { operator<(a, a); };\n",
            "template <typename T, typename U>\nconcept D = requires(T t, U u) { t.operator<(u); };\n",
            "template <typename T>\nvoid f(T* p) { p->~T(); }\n",
            // A **conditional handler**: a directive can land at either joint of a `try`, and the first one is the
            // one that used to break the statement — with `try` separated from its block, the block was not the
            // try's block at all, and the `catch` became a statement with no statement before it.
            "#if !defined(_DEBUG)\ntry\n#endif\n{\n    g();\n}\n#if !defined(_DEBUG)\ncatch (const E& e) {\n    h();\n}\n#endif",
            "void f() {\ntry {\n    g();\n}\n#if !defined(_DEBUG)\ncatch (const E& e) {\n#endif\n    h();\n}\n}",
            // The **same joint on an `if`**: a directive between a branch and its `else`. This is the standard
            // library's own shape — `bits/basic_string.h` line 490, where an `#if __cpp_lib_concepts` sits between
            // the then-branch and an `else if constexpr` — and it used to move the file's first error from there to
            // the `}` of a class a hundred lines away. See the seam comment in `parse_if_statement`.
            "if (n > 0)\n  g(n);\n#if FEATURE\nelse if (n < 0)\n  h(n);\n#endif",
            "if (n > 0) {\n  g(n);\n}\n#if FEATURE\nelse {\n  h(n);\n}\n#endif",
            // And between `else` and the statement it introduces.
            "if (n > 0)\n  g(n);\nelse\n#if FEATURE\n  h(n);\n#endif",
            // A directive **between two class members**, and around an access specifier. This is the shape that
            // made every standard-library class lose its members: the `#if` line used to be read as a *member
            // declaration* named after the condition's first identifier, and everything after it became a child of
            // that bogus member — lossless, diagnostic-free, and wrong. Measured: `std::basic_string` went from
            // 7 indexed members (all typedefs, no methods) to 117. See `docs/roadmap.md` §2.1.
            "struct S {\n  int a;\nprotected:\n#if FEATURE\n  int b;\n#else\n  int c;\n#endif\nprivate:\n  int d;\n};",
            "struct S {\n#if FEATURE\n  int a;\n#endif\n  int b;\n};",
            // The same seam **inside one declaration**, which is the other half of it and the one that decides
            // whether a standard-library class has a public interface at all. Seven shapes, each with its own
            // reading and each measured on `bits/basic_string.h`; the shape assertions are in
            // `a_directive_inside_a_declaration_keeps_the_members_after_it_members`, which counts the members the
            // class still has rather than asking whether the file parsed — a member read wrongly is silent.
            "struct S {\n  S()\n  noexcept(is_nothrow_default_constructible<A>::value)\n#if CONCEPTS\n  requires is_default_constructible_v<A>\n#endif\n  : _M_dataplus(_M_local_data())\n  { }\n};",
            "struct S {\n#if GUIDES\n  template<typename = _RequireAllocator<_Alloc>>\n#endif\n  S(const char* s);\n};",
            "struct S {\n#if CXX11\n  template<typename It, typename = std::_RequireInputIter<It>>\n#else\n  template<typename It>\n#endif\n  S(const It& first) { }\n};",
            "struct S {\n#if CXX11\n  template<class It, typename = std::_RequireInputIter<It>>\n  _GLIBCXX20_CONSTEXPR\n#else\n  template<class It>\n#endif\n  S& append(It first) { return *this; }\n};",
            "struct S {\n#if CXX20\n  [[deprecated(\"use shrink_to_fit() instead\")]]\n#endif\n  void reserve();\n};",
            "struct S {\n  _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR\n  bool\n  empty() const { return true; }\n};",
            "struct S {\n  template<typename _Tp>\n  _If_sv<_Tp, S&>\n  operator=(const _Tp& __svt) { return *this; }\n};",
        ],
    );

    // Statements and expressions.
    assert_reads(
        Where::Body,
        &[
            "Widget w(1, 2, 3);",
            "Widget w(g(), h());",
            "Widget w{a, b};",
            "int n = sizeof(int);",
            "auto n = alignof(int);",
            "auto d = static_cast<int>(1.5);",
            "auto d = reinterpret_cast<int*>(p);",
            "auto d = (int)1.5;",
            "auto [a, b] = pair;",
            "auto [a, b] = std::pair<int, int>{1, 2};",
            "for (auto&& [k, v] : m) { }",
            "auto g = [v = 1] { return v; };",
            "auto g = [v = total + 1] { return v; };",
            "auto g = [p = std::move(q)] { };",
            // A C++20 generic lambda: a template parameter list between the capture list and the parameters,
            // parsed by the same rule a class template uses.
            "auto g = []<typename T>(T t) { return t; };",
            "auto g = []<class T>(T t) { return t; };",
            "auto g = []<typename... Ts>(Ts... ts) { };",
            "auto g = []<typename T> { };",
            // Coroutine statements: `co_return` is `return`, `co_yield` is `throw`'s shape, and `co_await` is a
            // unary operator.
            "auto x = co_await f();",
            "co_await f();",
            "co_return 1;",
            "co_return;",
            "co_yield 1;",
            // The `template` disambiguator, in the three places a dependent name can be used. It exists to say
            // that the `<` after the name starts template arguments rather than a comparison.
            "auto x = T::template f<int>();",
            "auto x = obj.template f<int>();",
            "auto x = p->template f<int>();",
            // Pack *expansion* — the use half of a pack, whose declaration half is above. One rule covers all of
            // it: a `...` after a pattern expands it, wherever the pattern is written.
            "g(args...);",
            "g(h(ts)...);",
            "g(args..., more);",
            "auto n = sizeof...(Ts);",
            "auto t = std::tuple<Ts...>();",
            "auto t = std::tuple<Ts..., Us...>();",
            "auto s = static_cast<Ts&&>(args)...;",
            // Fold expressions: the same binary rule with the `...` read as an operand, in all four spellings.
            "auto n = (ts + ...);",
            "auto n = (... + ts);",
            "auto b = (ts && ...);",
            "auto b = (... && ts);",
            // A pack expansion in a capture list, which is how a variadic forwarding lambda is written. The
            // capture rule reads the capture itself rather than an expression, so it needed its own `...`.
            "auto g = [args...] { return g(args...); };",
            // Braced initialization of a temporary: the `T{...}` form, in every position an expression can
            // stand. This is the rule that made `auto v = Vec<int>{1, 2};` a declaration rather than an error.
            "auto v = Vec<int>{1, 2};",
            "auto s = std::string{\"x\"};",
            "auto e = Vec<int>{};",
            "return Vec<int>{1, 2};",
            "g(Vec<int>{1});",
            "auto sum = Vec<int>{1} + other;",
            "auto p = new Widget(1, 2);",
            "auto p = new Widget;",
            "auto p = new Widget();",
            "auto p = new int[4];",
            "auto p = new int[4]{1, 2, 3, 4};",
            "auto p = new int[];",
            "auto p = new int[2][3];",
            "auto p = new std::string(\"x\");",
            "auto p = new (buf) Widget();",
            "auto p = new (buf, size) Widget();",
            "auto p = new (1) Widget();",
            "auto q = new Widget(1)->run();",
            // The same allocations with the **global** qualification. `::new` is how a program asks for the global
            // `operator new` rather than a class's, so the tokens are not decorative — and the `::` used to send
            // the whole expression to the name branch, which reported a missing name and flattened it. See
            // `gaps::constructs_are_read_as_the_right_node` for the node that pins the reading.
            "auto p = ::new Widget(1, 2);",
            "auto p = ::new (buf) Widget();",
            "(void) ::new T;",
            "::new (__loc) _Tp[1]();",
            // The implementation's own spelling of `try`/`catch`, which libstdc++ writes in 18 of the closure's
            // files: `bits/exception_defines.h` defines `__try` as `try` and `__catch(X)` as `catch(X)`.
            "__try { g(); } __catch (...) { h(); }",
            "__try { g(); } __catch (const E& e) { h(e); }",
            // `throw(…)` after a parameter list — the C++98 dynamic exception specification, which C++17 removed
            // and the standard library's headers still write 63 times.
            "void f() throw();",
            "void g() throw(int);",
            "void h() const throw(std::exception&, int);",
            "int __cdecl f(void) throw();",
            // A **macro between `if` and its condition**, which is how `if constexpr` is written in a header that
            // must also compile as C++14: `_GLIBCXX17_CONSTEXPR` is `constexpr` or nothing at all.
            "if _GLIBCXX17_CONSTEXPR (x) { }",
            "if (_GLIBCXX17_CONSTEXPR (x)) { }",
            "if (x) { } else if _GLIBCXX17_CONSTEXPR (y) { }",
            // `[[likely]]` on the substatement, which is where C++20 puts it.
            "if (x) [[likely]] { }",
            "if (x) [[unlikely]] y();",
            "if (x) [[likely]] { } else [[likely]] { }",
            // A template head *wraps* the declaration it introduces, and three things follow from that being read
            // as a head rather than as tokens in front of one. Each was a separate defect, and the census of the
            // standard library found all three:
            //
            // * a **partial specialization** names its template with a template-id in the *name* position —
            //   `v<T*>` — which is what the rule refusing a bare template-id there used to refuse;
            // * an **unnamed parameter whose type is an array** — `f(T[4])`, whose `[4]` is a declarator suffix
            //   that opens only when "does this declaration lead with a type keyword" answers yes — and that
            //   question was answered by walking back past the head, where the first token is `template`;
            // * an **array type as a template argument** — `S<T[N]>`, which is a type only if `T` is one, and `T`
            //   is a template parameter: a name the file declares in type position, scoped to its declaration.
            "template <typename T> constexpr bool v = false;",
            "template <typename T> constexpr bool v<T*> = true;",
            "template <> constexpr bool v<int> = true;",
            "template <typename T, int N> struct S<T[N]> { };",
            "template <typename T> struct S<T[]> { };",
            "template <typename T, int N> constexpr bool d<T[N]> = true;",
            "template <typename T> void f(T[4]);",
            "template <typename T> void f(T[]);",
            "template <template <class> class C, class T> struct S<C<T>> { };",
            "template <class T> struct Outer { template <class U> void f(T t, U u[4]); };",
            // A **linkage specification's block is not a body**: the declarations inside are at file scope, so the
            // macro-from-a-header rules keep working in there. Most of libstdc++ is written inside
            // `extern "C++" { namespace std { … } }`, and counting that block as a body is what made
            // `_GLIBCXX_BEGIN_NAMESPACE_VERSION` read as an ordinary name.
            "extern \"C++\"\n{\nnamespace std\n{\n_GLIBCXX_BEGIN_NAMESPACE_VERSION\n  int helper();\n}\n}",
            "delete[] p;",
            "int n = sizeof(void(int));",
            "auto x = a ? b : c;",
            // The C++17 initializer in a condition: a declaration, a `;`, and the condition itself.
            "if (auto q = find(x); q != nullptr) { }",
            "switch (auto q = f(); q) { }",
            "if (x > 0) { }",
            "if (Foo* p = get()) { }",
            "if (auto q = f(); q) { } else { }",
            "try { } catch (const E& e) { }",
            "goto label; label: ;",
            "int x = {1};",
            // A **braced-init-list** where the grammar says *initializer-clause* rather than *expression*: the
            // right-hand side of an assignment and an argument of a call. Both were reached only after the
            // empty-declarator defect was fixed, because `x = {1};` used to be read — silently, and wrongly — as
            // a declaration of a variable whose declarator named nothing. See `expressions.rs`.
            "x = {1};",
            "x = {1, 2};",
            "x = {};",
            "x += {1};",
            "v.push_back({1, 2});",
            "f({1});",
            "f(a, {1});",
            // A **requires-expression**, which is one of the three places a braced-init-list is *not* read: a
            // `{` inside a constraint opens a requirement.
            "auto r = requires { g(); };",
            "if constexpr (requires { g(); }) { }",
            "static_assert(requires { g(); });",
            // A parenthesised variable is an expression, not a cast — the shape that made the cast rule
            // precise rather than eager.
            "x = (a);",
            "x = (a + b);",
            "x = ((a));",
            "x = a * (b + c);",
            // The **comma operator**, in the positions where a comma really is an operator rather than
            // punctuation. The container positions are pinned by `a_comma_in_a_container_is_still_a_separator`
            // in `operators.rs`, because those are the ones this could take away.
            "auto x = (a, b);",
            "x = 1, y = 2;",
            "return 1, 2;",
            "for (a = 0, b = 0; ; ) { }",
            "g(a, (b, c));",
            // The **C-style cast**. `(T*)p` needs no type table, because a `*` that closes the parentheses has
            // no right operand and therefore cannot be a multiplication. The bare-name form, `(MyType)1.5`, was
            // the half that stayed a documented trade-off for longer — what retires it is the token *after* the
            // `)`: an operand there cannot follow an expression, so the parentheses held a type.
            "auto d = (T*)p;",
            "auto d = (MyType*)p;",
            "auto d = (T&)x;",
            "auto d = (ns::T*)p;",
            "auto d = (T<int>*)p;",
            "auto d = (const T*)p;",
            "auto d = (T*)p->q;",
            "auto d = (T*)p + 1;",
            "g((T*)p, (U*)q);",
            "auto d = (MyType)1.5;",
            "auto d = (MyType)x;",
            "auto d = (size_t)size;",
            "buf = (char *)malloc((size_t)size + 1);",
            "auto d = (MyType)new T;",
            "auto d = (MyType)sizeof(T);",
            // **Adjacent string literals**, which are one literal rather than a grammar rule — translation phase
            // 6. Reading one and stopping ended the initialiser after the first, so this is how every long message
            // wrapped across lines used to fail.
            "auto s = \"a\" \"b\";",
            "auto s = \"a\" \"b\" \"c\";",
            "g(\"a\" \"b\", 1);",
            // A **user-defined literal** in an expression: the lexer named the kind so the parser would not read
            // it as a plain number, and no rule was reading it at all.
            "auto x = 1_km;",
            "auto x = \"a\"_km;",
            // A `for` header's **step**, which is an expression: the declaration reading had no `;` to fail on
            // and consumed the name as a type, so `i++` was never read at all.
            "for (;; i++) { }",
            "for (;; i++, k++) { }",
            "for (i = 0, k = 0; i < n; i++, k++) { }",
            // A **`sizeof` whose operand is not a bare name**. The type reading was accepted as soon as it parsed
            // and consumed something, and a type-id can stop early — a name alone is a complete one — so the
            // cursor was left on the `[` and `expected )` was reported against it. Everything but a bare name and
            // a keyword type failed, which in C is most of the `sizeof`s there are.
            "auto n = sizeof(a[0]);",
            "auto n = sizeof(a.b);",
            "auto n = sizeof(a + b);",
            "auto n = sizeof(a());",
            "auto n = sizeof(int*);",
            "auto n = sizeof(unsigned long);",
            "auto n = typeid(a[0]);",
            // An **elaborated type specifier** in a type-id: `struct S` is one specifier, so the name after the
            // keyword belongs to the type even where a type-id allows only one name — which is why `sizeof(struct
            // S)` and `(struct S*)p` used to leave the name behind and never reach the `)`.
            "auto n = sizeof(struct S);",
            "auto n = sizeof(union U);",
            "auto n = (struct S*)p;",
            "using A = struct S;",
            // An **array type** in a type-id, and the same tokens as an index — the file's own table is what tells
            // them apart. Beside them the shapes that must stay expressions.
            "auto n = sizeof(int[4]);",
            "auto n = sizeof(char[256]);",
            "auto n = sizeof(int[2][3]);",
            "auto n = sizeof(int*[4]);",
            "auto n = sizeof(int[]);",
            "Vec<int[4]> v;",
            "auto v = Vec<int[4]>{};",
            "auto x = a[b < c];",
            // A **cv-qualifier after the type**: `char const w[]` used to read `w` into the type and turn the
            // declarator into a structured binding of nothing — silently — and `char const w[2]` reported it.
            "char const w[] = { 'a' };",
            "char const w[2] = { 'a' };",
            "int const x = 1;",
            "unsigned const int y = 1;",
            "struct S const s;",
            // **Qualified declarator names**: a definition's name is folded into the type, so its parentheses are
            // a parameter list and nothing else — with and without a storage specifier in front, and with a
            // template-id in the name. The questions that decide this used to be asked of the declaration's first
            // token, which a `static` pushes out of the way.
            "static void A::f<int>(int);",
            "inline void A::f<int>(int);",
            "extern void A::f<int>(int);",
            "static void A<int>::f<int>(int);",
            "static void A::f<int>(int) { }",
            "static void Widget::draw(T) { }",
            "void Widget::draw(T) { }",
            "void ns::C::method() { }",
            // `<` as a **comparison** rather than a template-id opener — the reading is speculative, and what
            // decides it is the token after the list (see `operators.rs`). `if (n < 0 || n > 100000)` is how a
            // range check is written, and the old lookahead paired the `<` of the first comparison with the `>`
            // of the second. Beside them: the template-ids that must keep their reading.
            "x = a < b > c;",
            "x = a < b || c > d;",
            "if (n < 0 || n > 100000) { }",
            "while (i < n && j > k) { }",
            "g(a < b, c > d);",
            "auto n = A<B>::value;",
            "g<int>(1);",
            "auto p = new A<B>();",
            "auto v = std::vector<int>{};",
            "std::vector<int> v;",
            "Foo<int> x;",
            // **Alternative operator spellings** — the `<iso646.h>` names, which are real C++ rather than an
            // extension. They arrive as identifiers, because the lexer has no keyword for them.
            "auto x = a and b;",
            "auto x = a or b;",
            "auto x = not a;",
            "auto x = a bitand b;",
            "auto x = a bitor b;",
            "auto x = a xor b;",
            "auto x = compl a;",
            "auto x = a not_eq b;",
            "a and_eq b;",
            "a or_eq b;",
            "a xor_eq b;",
            "if (a and b) { }",
            "while (not done) { }",
            // **throw-expressions**: `throw` where a *value* is expected, as opposed to the statement form. The
            // two are different nodes; see `modern.rs`.
            "x = throw 1;",
            "auto y = cond ? throw 1 : 2;",
            "return throw 1;",
            "g(throw 1);",
            // **Explicit instantiation declarations**, which are `extern template`, not a linkage specification.
            "extern template struct S<int>;",
            "extern template class C<int>;",
            "extern template void f<int>(int);",
            // …and the same construct **without** `extern`, which the standard writes as one production:
            // `explicit-instantiation: extern(opt) template declaration`. A function's template-id is its *name*,
            // a class's is its *head*, and a variable template's is its name again.
            "template void f<int>(int);",
            "template void f<int>(int) { }",
            "template class C<int>;",
            "template struct S<int>;",
            "template int v<int>;",
            // **Explicit specializations**, whose empty head is what tells them from a template declaration. The
            // declaration they introduce names a specialization, so its declarator is a template-id too.
            "template <> void f<int>(int);",
            "template <> int v<int>;",
            "template <> struct S<int>;",
            // **Inline namespaces**, whose members are also members of the enclosing namespace.
            "inline namespace v1 { }",
            "inline namespace v1 { int x; }",
            "inline namespace v1 = a::b;",
            // **`decltype` in type position**, in every position a type can be written. The tricky part is not
            // the payload but what follows it: a `)` ends the specifier, and the declarator's name comes after.
            "decltype(x) y;",
            "decltype(x) y = 1;",
            "decltype(auto) y;",
            "decltype(auto) x = f();",
            "decltype(x)* p;",
            "decltype(x) v[2];",
            "noexcept(f()) g();",
            "const decltype(x) y = 1;",
            // **`concept` and `requires`** — the largest single block of missing grammar. Four constructs share
            // the one word: a constrained template parameter, a requires-clause after the template parameter
            // list, the same clause after a declarator, and a requires-expression, which is a primary expression
            // and can therefore be nested anywhere an expression can.
            "template <typename T> concept C = true;",
            "template <typename T> concept C = requires(T t) { t.f(); };",
            "template <typename T> concept C = C2<T> && C3<T>;",
            "template <C T> void f(T t);",
            "template <C<T> U> void f(U u);",
            "template <typename T> requires C<T> void f(T t);",
            "template <typename T> requires C<T> && C2<T> void f(T t);",
            "template <typename T> requires (C<T>) void f(T t);",
            "template <typename T> void f(T t) requires C<T>;",
            "template <typename T> void f(T t) requires C<T> { }",
            "template <typename T> void f(T t) requires requires(T t) { t.f(); };",
            "void f() requires true;",
            // The clause **after a trailing return type**, which is the only order the standard allows: the
            // clause belongs to the init-declarator, so it follows the whole declarator, the `-> T` included.
            "template <typename T> auto g(T t) -> int requires C<T>;",
            // A parenthesised constraint — which the standard *requires* around anything that is not a
            // conjunction of primary expressions: `requires (N > 0)` is valid and `requires N > 0` is not.
            "template <typename T> requires (sizeof(T) > 1) && (sizeof(T) < 64) void h(T t);",
            "template <int N> requires ((N >> 1) > 0) void h();",
            // …and a comparison **inside parentheses** in a clause, where the operand after the `>` belongs to
            // the constraint rather than to the declaration that follows the clause.
            "template <int N> requires (N < 0 || N > 3) void f();",
            "template <int N> requires (N > 0) void f();",
            "template <typename T> requires (C<T>) T value = T{};",
            "template <typename T> void f(T t) requires C<T> && C2<T> { }",
            // A function whose parameter is **unnamed**: `void f(T)`, not `void f(T t)`. The same tokens as a
            // direct-initialised variable, and at file scope the variable reading used to win — silently, since
            // a variable declaration with a direct initialiser is perfectly well formed. See `direct_init.rs`.
            "void f(T);",
            "void f(T) { }",
            "template <typename T> void f(T) { }",
            "template <typename T> void f(T) requires C<T> { }",
            "void f(std::vector<T>);",
            // The same two words used as **ordinary names**, which is what says they are contextual keywords
            // rather than keywords: the lexer hands both over as identifiers and the grammar asks for the spelling
            // where the standard gives it a meaning. See `concepts.rs`.
            "int requires = 1;",
            "int concept = 2;",
            "void requires();",
            "void f(int requires, int concept);",
            "struct S { int requires; int concept; };",
            // A **K&R (old-style) function definition**, whose parameters are declared *after* the parenthesis
            // rather than inside it. It is C's spelling from before 1989, and the file that found it is a CMake
            // compiler-id probe — generated into every CMake build tree, so it is not an exotic file. Both the
            // definition and the body-less head are listed, because a conditional can put the head in one branch
            // and the body in neither; `old_style.rs` owns the shapes, this list only says that they are read.
            "int main(argc, argv)\nint argc;\nchar *argv[];\n{ return argc; }",
            "int main(argc, argv)\nint argc;\nchar *argv[];",
            "int f(a)\nint a;\nfloat b;\n{ return a; }",
            // Enumerators **with initializers**, which is where the enumerator list is really a list: a reader
            // that takes the comma swallows the members after the first initialised one (B26), and an enum whose
            // members have no initializers cannot see that. `enum Color { Red, Green }` was the example for years.
            "enum E { A = 0, B };",
            "enum E { A = 0, B, };",
            "enum class E : unsigned { A = 1, B = 2, C };",
            "enum E { A = 1 << 2, B = f(1, 2), C };",
            // A **byte-order mark**: not a construct, but the first character of a file Visual Studio saved, and
            // it used to be the first *diagnostic* of that file too (37 of 200 files in a real project).
            "\u{feff}int x;",
            // An **export macro** in front of the type: two names where a declaration has its type, and the
            // declarator is the third. `ast.rs` pins the name it comes out with; this only says it is read.
            "MY_API Widget *p;",
            "EMMY_API Result f(int x);",
            "EXPORT std::string g();",
            // Two closers in one token. The lexer glues `>>` together (`a >> b` is a shift), so a nested
            // template-id ends with a token that closes **two** lists — and the empty argument list
            // `std::less<>` has no argument to read before the closer arrives, which is why it failed while the
            // non-empty spelling worked. Any scan that counts angles has to answer for `>>`; see
            // `angle_depth_delta`.
            "class D : public Base<K, std::shared_ptr<V>> { };",
            "class D : public std::vector<std::pair<K, V>> { };",
            "void f(const std::map<int, int, std::less<>> &m);",
            "std::map<int, int, std::less<>> &Get();",
            "void f(std::map<int, std::less<int>> m);",
            // **Pointers to members**, in every spelling: the operator after the type (`C::*`), the parenthesised
            // form with a name, the abstract form a parameter uses, and the expression operators. `gaps.rs` also
            // pins the *shape* below, because the unparenthesised form used to parse with the declarator nested
            // inside the type — lossless, well formed, no diagnostic, wrong.
            "int C::*p;",
            "typedef int (C::*fp)(int);",
            "void g(int (C::*h)(int));",
            "void h(int (C::*)(int));",
            "struct C { };\nvoid f(C* c) { (c->*h)(1); }",
            "struct C { };\nvoid f(C& c) { (c.*h)(1); }",
            // The literal suffixes those arguments use — `1k_row` is one token, not a number and a name. The
            // macro-definition shape itself is in the file-scope list above: inside a body it is a call followed
            // by a block, which is a real error and is refused on purpose.
            "auto x = 1k_row;",
            // A **macro whose body is a whole statement**, invoked without a `;`. The `#define` in the fragment is
            // what makes it a macro rather than a call whose `;` is missing — `macros.rs` owns both halves of that
            // test, and the negative half is pinned in the list of what is not read yet.
            "#define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { }\nNUMBER_OPTION(tab_width)",
            "#define IF_EXIST(op) if (!Get(op).empty())\nIF_EXIST(a) { g(); }",
            // A **macro invocation used where a definition goes, inside a function body** — `IF_EXIST(k) { … }`,
            // which is how a "set this option if it is configured" block is written. Inside a body the same shape
            // is also a real mistake (a call whose `;` is missing, followed by a block), so the reading is taken
            // only for a name spelled the way a macro is — see the negative pin in the list of what is not read
            // yet, which is the other half of this one.
            "IF_EXIST(k) { g(); }",
            "TEST(A, B) { g(); }",
            // A **function type as a template argument** — a predicate parameter, in a declaration, an alias and a
            // `sizeof`. The angle-matching scan stopped at the first `)`, so the template-id was never attempted:
            // `A<bool(T)> x;` came out as the *comparison* `A < bool(T) > x`, with no diagnostic at all.
            "A<bool(T)> x;",
            "std::function<bool(TokenKind)> pred;",
            "using F = A<bool(T)>;",
            "auto n = sizeof(A<bool(T)>);",
            "void h(A<bool(T)> x);",
            // A **class-like definition followed by its declarator** — `struct S { … } x;`, and the C idiom
            // `static const struct { … } name[] = { … };`. The body is a complete type, but the backward walk for
            // "is the type complete?" only saw the `}`, so the declarator's name joined the *type* instead: the
            // declaration declared nothing, silently, and reported `expected a declarator name` as soon as the
            // declarator carried anything (`x = { 1 }`, `x[2]`).
            "struct S { int a; } x;",
            "struct { int a; } x[] = { { 1 } };",
            "static const struct { unsigned char left; } priority[] = { { 1 } };",
            "typedef struct { int a; } Alias;",
            "enum E { A } e;",
            "class C { int a; } c = {};",
            // A **global-qualified name in parentheses** is an expression; the `::` used to make it a cast type.
            "auto y = (::x);",
            "auto y = (::abs(x) > 1);",
            // A **functional-notation conversion with a keyword type** — `bool(x)`, the other way of writing
            // `(bool)x`.
            "auto y = bool(x);",
            "g(\"key\", bool(lint[\"codeStyle\"]));",
            "auto timeout = 100ms;",
            "auto text = \"name\"sv;",
        ],
    );

    // Class members. The class body is `struct Probe { ... }`, so a constructor written here has to be
    // called `Probe` — a constructor whose name is not its class's does not exist in C++, and it was the
    // test that was wrong rather than the parser when this list was first written.
    assert_reads(
        Where::Class,
        &[
            "mutable int cache = 0;",
            "[[nodiscard]] int pure() const noexcept;",
            "friend void swap(S&, S&);",
            "virtual ~S() = default;",
            "void f() override;",
            "static inline int count = 0;",
            // Inside a **member function's body** the class body is no longer the innermost brace, and a `:` there
            // is never a bit-field's width: a range-`for`'s separator and a label were both read as widths, which
            // cost a whole header in a real project.
            "void f() { for (auto &v: vec) { } }",
            "void f() { again: g(); }",
            "Probe(int v) : a(v) {}",
            "Probe() : a(1) {}",
            "Probe() : b{2} {}",
            // Destructors, in every spelling: declared, defined, defaulted, deleted, virtual, and written
            // out of line with a qualified name.
            "~Probe();",
            "~Probe() {}",
            "virtual ~Probe() = default;",
            "~Probe() = delete;",
            "~Probe() noexcept {}",
            "template <typename T> void f(T t);",
            // A member alias template: the same head-and-`using` shape as the file-scope form, written where
            // the member rule reaches it.
            "template <typename U> using rebind = Probe<U>;",
            "struct Nested { int x; };",
            "enum Color { Red, Green };",
            // Bit-fields, including a run of them and a nameless one.
            "int bits : 3;",
            "unsigned flags : 1, spare : 7;",
            "int : 0;",
            // A using-declaration of a base member, in the two shapes it is written in.
            "using Base::method;",
            "using Base::Alias;",
            "using Alias = int;",
            // A `friend` declaration *followed by another member*. The friend is a specifier whose payload is
            // the whole declaration — `;` included — so the member written after it is where the old rule ran
            // out of tokens and turned everything into error nodes.
            "friend void swap(Probe&, Probe&); int after = 0;",
            "friend class Other; int later = 0;",
            // The same shape with a *declarator* after it rather than another whole declaration, which is the
            // half the first pair does not reach: `Probe& method();` is a type, a reference and a name, and the
            // specifier sequence has to know that the type was named even though the friend consumed it.
            "friend void swap(Probe&, Probe&); Probe& method();",
            "friend class Other; Probe& method2();",
            // An **explicit object parameter** (C++23). It was a *silent* gap: the member came out as nothing
            // and the tokens after the `this` were read as a second, phantom member, with no diagnostic — which
            // is why this list could not see it and a census of common C++ found it.
            "void f(this Probe& self);",
            "void f(this Probe&& self) &&;",
            "void f(this auto&& self) {}",
            "void g(this Probe& self, int x);",
            "void h(this Probe& self) const;",
            "void i(this Probe&);",
            "void j(this Probe& self) { self.x = 1; }",
        ],
    );
}

#[test]
fn constructs_the_parser_does_not_read_yet() {
    // Each entry is `(construct, why not)`. The reason is the important half: it says whether the gap is a
    // missing rule that would be cheap to add or a decision that needs an ambiguity resolved first.

    assert_does_not_read_yet(
        Where::Body,
        &[
            // Nothing is left of the C-style cast that used to be here, and the record of why is worth keeping —
            // it is the third time a "deliberate trade-off" turned out to be a missing rule.
            //
            // The **pointer form** was here first, described as "the canonical example of a deliberate trade-off:
            // `*` is both the pointer operator and the multiplication operator, and the type table is what tells
            // them apart". That claim was wrong twice over:
            //
            //   `(a * b)` and `(MyType*)p` are not the same shape — `a * b` has an operand on both sides of the
            //   `*`, `MyType*` has nothing on its left — so the difference **is** visible in the tokens; and
            //
            //   a `*` immediately before the `)` cannot be a binary operator at all, because a binary operator
            //   needs a right operand.
            //
            // The **bare-name form** — `(MyType)1.5`, `(MyType)x` — was the second half of the same entry, and
            // the same argument retires it: what *follows* the `)` decides. Two operands in a row is not an
            // expression in any grammar, so `)` followed by an identifier, a literal or a prefix keyword means the
            // parentheses held a type. It was found in a real C file, where `(size_t)size` is not exotic but
            // routine.
            //
            // What genuinely remains is the token that means something in *both* grammars, and it is not a gap:
            // `(a)*p` is a multiplication, `(a)-b` a subtraction, `(a)(b)` a call. All are valid expressions, so
            // the cast reading would take working code apart. See `an_operand_after_the_parentheses_decides_the_
            // cast_and_nothing_else_does` in `operators.rs`.
        ],
    );

    // …and the shapes that are **still errors**, which is the other half of the macro reading: inside a
    // function body a call whose `;` is missing, followed by a block, is not a macro definition — the name is
    // not spelled like one — and reporting it is the point. The convention buys the macro case and keeps the
    // typo.
    assert_does_not_read_yet(
        Where::Body,
        &[
            (
                "g(x) { }",
                "a call with its `;` missing, followed by a block — not a macro, since the name is not spelled \
                 like one; see B36 in docs/grammar-gaps.md",
            ),
            (
                "NUMBER_OPTION(tab_width)\ng(x)",
                "a call with its `;` missing and **no `#define` in the file**: the macro reading needs that \
                 evidence, and a spelling convention is not enough for it; see B41 and `macros.rs`",
            ),
            // `if (int x = g()) { }` used to be here — a condition that declares a variable. It was read as
            // `expected primary expression` against the `int`, and the block after it became rubble. The `for`
            // header's declaration rule is what fixed it (that rule has no `;` of its own, which is exactly the
            // shape a condition needs); `a_condition_may_declare_a_variable` now pins the reading, including the
            // **silent** half of the same defect that this entry could not see:
            //
            //   if (Foo* p = get())   was a `BinaryExpr` — `Foo * p = get()` — with no diagnostic at all
            //
            // A "does it parse?" list cannot catch that one, which is the second number of maintenance
            // convention 29 and the reason the shape assertions below exist.
        ],
    );

    assert_does_not_read_yet(
        Where::File,
        &[(
            "void f() try { } catch (...) { }",
            "a **function-try-block**: `try` written between the declarator and its body. Valid C++ (checked \
             with g++ 15), read today as four errors starting at `void`. The `try` was given a rule as a \
             *statement* (that is why the same tokens are fine inside a body) and not as a part of a function \
             definition",
        )],
    );

    // `decltype` in **type position** used to be here, and the record of what it looked like is worth keeping
    // because the symptom pointed at the wrong thing entirely:
    //
    //   `decltype(x) y;`          read
    //   `decltype(x) y = 1;`      did NOT — "expected primary expression" against the `decltype`
    //   `decltype(auto) x = f();` did NOT
    //
    // Two defects were stacked, and neither was where the message pointed:
    //
    //   1. `decltype` was not a declaration anchor, so the declaration reading was never taken from it at all;
    //   2. once it was, the specifier loop asked "did this specifier name a type?" of the **last token it
    //      consumed** — which for `decltype(a)` is the `)` of its payload, the very token `alignas(16)` ends on
    //      and the one that question exists to answer *no* for. So the type was left unfinished as far as the
    //      loop was concerned, the **declarator's own name** was taken into the type, the declaration came out
    //      with no declarator, and the statement fell back to an expression. The initializer had nothing to do
    //      with it.
    //
    // Both are fixed; the entries are in the list above, and `modern.rs` pins the shapes. What remains is the
    // **expression** side, which is the one place the tokens really are silent:
    //
    //   `decltype(x);`      refuses — a `decltype` is not a value
    //   `decltype(x) + 1;`  refuses — likewise
    //
    // Those are refused rather than misread, which is the cheaper direction and the one this file registers.

    // The record of how the pointer form was resolved, kept because the *reason* it was mis-filed for so long
    // is the useful part: it was judged by how hard it looked rather than by whether it needed information from
    // outside the file.
    //
    //   `auto d = (MyType*)p;`   — reads: the `*` closes the parentheses, so it is a type
    //   `auto d = (T*)p;`        — likewise
    //   `auto d = (int)1.5;`     — reads through the keyword-type path
    //   `auto d = (a * b);`      — reads as a multiplication, and must keep doing so
    //   `auto d = (a* b);`       — likewise

    // Pack expansion used to be here, and it is worth recording what the entries were, because they looked
    // like three separate gaps and were one rule — "a `...` after a pattern expands it":
    //
    //   `g(args...)` — an argument list;
    //   `sizeof...(Ts)` — which additionally needed the lexer's two tokens (`sizeof`, `...`) joined, since it
    //   has no token of its own;
    //   `std::tuple<Ts...>` — a template argument, where the type reading stops at the ellipsis and the list's
    //   loop then read it as an argument of its own.
    //
    // The declaration half of packs was never a gap (`template <typename... Ts> void f(Ts... ts);` has been in
    // the list above since it was written). Fold expressions came with the same change: `(ts + ...)` is the
    // ordinary binary rule with the `...` read as its right operand.

    // `alignas` used to be here, with the note that "the specifier loop has no branch for it, so the `(` is read
    // as an expression and the declaration reading is refused".
    //
    // What it actually needed was three things, and only the first was the missing branch: the specifier itself
    // (`AlignasSpec`, holding a payload that is a constant-expression *or* a type-id), `alignas` as a declaration
    // anchor, and — the one that was not obvious — the specifier loop's "has a specifier been seen?" flag split
    // into "has a **type** been seen?", because `alignas(16) MyType value;` consumes a specifier and names no
    // type. Without the split the name was read as a second word of a finished type.

    assert_does_not_read_yet(
        Where::Class,
        &[
            // Everything that was here is read now. The entries were:
            //
            //   `int bits : 3;` — a bit-field, which needed the class-body context to tell its `:` from a
            //   constructor's member-initializer list;
            //   `using Base::method;` — a using-declaration, which needed the declaration rule to dispatch
            //   `using` at all (at file scope the statement rule happened to claim it first);
            //   `union Value { int i; ~Value() {} };` — a destructor definition, which needed the tilde
            //   claimed before the specifier sequence could read it as an operator.
            //
            // They are pinned as *read* in the test above instead, and this list is kept — rather than deleted —
            // so that the next gap has a place to go.
            //
            // The next gap: a **function-try-block**, whose `try` sits between the declarator and the body.
            // `try { } catch (const E& e) { }` as a *statement* is read (it is in the list above); the same words
            // in a function's own position are not, and the declarator's tail accepts `{`, `;`, `:`, `requires`
            // and an old-style parameter list — not `try`. Found while writing `old_style.rs`, whose list of
            // "what a modern function declarator continues with" was meant to contain no unsupported spelling.
            // Rare outside constructors, and loud rather than silent, so it is logged rather than fixed.
            (
                "void f() try { } catch (const E& e) { }",
                "the declarator's tail has no rule for `try` — see B25 in docs/grammar-gaps.md",
            ),
        ],
    );
}

/// What the parser read each construct **as**, not merely whether it read it.
///
/// The third question, and the one the other two are blind to. See [`Shape`] for why a well-formed, lossless,
/// diagnostic-free tree can still be the wrong construct, and for the case that was found that way.
///
/// The entries below are the statement kinds that matter to a consumer. They are deliberately not "every
/// statement parses": each line is a reading that a defect could silently change, so the list grows when a new
/// ambiguity is settled rather than when a new construct is added.
#[test]
fn constructs_are_read_as_the_right_node() {
    assert_statement_kind(&[
        // The defect this list exists for. An assignment to a name the file has no type for used to come out as
        // a `Declaration` whose declarator named nothing — the same reading as `int x = 1;`, which is a
        // *declaration*, so the two had to be told apart by whether an initialiser has something to initialise.
        shape("x = 1;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = a + b;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("value = other;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = f();", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = a ? b : c;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = { 1 };", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = { 1, 2 };", Where::Body, CppSyntaxKind::ExpressionStat),
        // Two template-ids joined by `&&`. The declaration reading is available — `C<T>` is a type and `&&` an
        // rvalue reference — and it used to win, producing a *declaration* whose declarator was named `C2<T>`:
        // well formed, lossless, and no diagnostic. A declarator's name cannot have template arguments, which is
        // what settles the reading. See `expressions.rs`.
        shape("C<T> && C2<T>;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = 1;", Where::File, CppSyntaxKind::ExpressionStat),
        // A **function type as a template argument**, where the wrong reading is invisible: the angle-matching scan
        // stopped at the parameter list's `)`, the template-id was never attempted, and `A<bool(T)> x;` came out as
        // the comparison `A < bool(T) > x` — an `ExpressionStat` that a consumer reads as "a statement with no
        // declaration in it", with nothing reported. The declaration reading is the assertion.
        shape("A<bool(T)> x;", Where::File, CppSyntaxKind::Declaration),
        shape(
            "std::function<bool(TokenKind)> pred;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        // A **class-like definition in front of its declarator**: `struct S { … } x;` swallowed `x` into the type
        // and declared nothing — silently when the declarator was bare, loudly as `expected a declarator name` as
        // soon as it carried an initializer or a bound.
        shape(
            "struct S { int a; } x;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        shape(
            "static const struct { int a; } priority[] = { { 1 } };",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        // The other side of the same gate: a declaration *is* a declaration, and a qualified declarator keeps
        // its declaration reading even though it names nothing for the declarator rule to take — the name was
        // folded into the type.
        shape("int x = 1;", Where::Body, CppSyntaxKind::Declaration),
        shape(
            "int ns::count = 0;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        shape(
            "int ns::Widget::count = 0;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        // Direct-initialisation, the reading the whole file-local type table exists for.
        shape("Widget w(1, 2);", Where::Body, CppSyntaxKind::Declaration),
        shape(
            "std::string s(\"x\");",
            Where::Body,
            CppSyntaxKind::Declaration,
        ),
        // A call keeps its parentheses, which is the direction a naive fix breaks.
        shape("g(1, 2);", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("use(x);", Where::Body, CppSyntaxKind::ExpressionStat),
        // Statements that are not declarations at all, pinned so that a future gate cannot claim them.
        shape("++i;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("delete p;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("if (x) { }", Where::Body, CppSyntaxKind::IfStat),
        shape("for (;;) { break; }", Where::Body, CppSyntaxKind::ForStat),
        shape("return;", Where::Body, CppSyntaxKind::ReturnStat),
        // **The comma operator**, on both sides. The required half is that the comma really is an operator
        // where it is an operator; the forbidden half is the defect it could cause — a container's commas read
        // as one expression, so `g(a, b)` becomes a call with one argument. The statement kind is the same in
        // both readings, which is exactly why the expression assertion exists.
        //
        // `auto x = (a, b);` would *not* do for these: `auto` is a type keyword, so the whole thing is a
        // declaration and the statement kind says nothing about the comma. The parenthesised form is used
        // instead, where the expression *is* the statement.
        requiring(
            shape("(a, b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("x = 1, y = 2;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("return 1, 2;", Where::Body, CppSyntaxKind::ReturnStat),
            CppSyntaxKind::BinaryExpr,
        ),
        forbidding(
            shape("g(a, b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        forbidding(
            shape("g(a, b, c);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        // **The C-style cast**, on both sides. `(T*)p` is a cast because a `*` that closes the parentheses has
        // no right operand; `(a * b)` is a product because its `*` has one. Both parse, so only the shape can
        // tell them apart.
        requiring(
            shape("(T*)p;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        requiring(
            shape("(MyType*)p;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        forbidding(
            shape("(a * b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        forbidding(
            shape("(a* b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // `(a)` parses as a type-id — one name, no declarator — so a rule that tried the type reading whenever
        // it could would turn every parenthesised variable into a cast.
        forbidding(
            shape("(a);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // And a call of a parenthesised name keeps its arguments, which is the price of the bare-name cast
        // staying unread.
        forbidding(
            shape("(f)(x);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // **throw** has two forms and they are different nodes. A consumer looking for one must not find the
        // other, and the statement form would look plausible in either position.
        requiring(
            shape("x = throw 1;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::ThrowExpr,
        ),
        requiring(
            shape("throw 1;", Where::Body, CppSyntaxKind::ThrowStat),
            CppSyntaxKind::ThrowStat,
        ),
        forbidding(
            shape("x = throw 1;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::ThrowStat,
        ),
        // **`::new`**, the global allocation. The statement kind alone would not tell it from the unqualified
        // form — both are an `ExpressionStat` holding a `NewExpr` — so the assertion is the `NewExpr`, and the
        // qualification is checked separately below. What the wrong reading produced was not a different node but
        // *no* node: `::` sent the tokens to the name branch, which reported a missing name at `new` and left the
        // whole allocation as one flat `IdentifierExpr`.
        requiring(
            shape("::new (buf) Widget();", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::NewExpr,
        ),
        // An **alternative spelling** is the operator, so the node is the one the punctuation would produce.
        requiring(
            shape("auto x = a and b;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("auto x = not a;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::UnaryExpr,
        ),
        // A name that merely looks like a spelling stays a name.
        forbidding(
            shape("auto x = android;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::BinaryExpr,
        ),
        // `this` as an **argument** is an expression; only a parameter position makes it a type.
        forbidding(
            shape("g(this);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::Parameter,
        ),
        // `decltype` in type position is a **declaration**, and the tricky half is that the spelling alone does
        // not say so — `decltype(x) + 1;` is an expression. The anchor asks whether a declarator follows.
        requiring(
            shape(
                "decltype(x) y = 1;",
                Where::File,
                CppSyntaxKind::Declaration,
            ),
            CppSyntaxKind::DeclSpecifierSeq,
        ),
        requiring(
            shape("decltype(auto) y;", Where::File, CppSyntaxKind::Declaration),
            CppSyntaxKind::DeclSpecifierSeq,
        ),
    ]);

    // The other half of the array assertion above, and the half that keeps it from being a licence: a name that
    // is *not* a template parameter keeps its subscript. `a[I]` is an expression, and an `ArrayType` here would be
    // a type the file never wrote.
    assert!(
        !contains(
            "template <int I> struct S<a[I]> { };",
            CppSyntaxKind::ArrayType
        ),
        "`a` is not a template parameter, so `a[I]` is a subscript"
    );

    // The `::` of a `::new` belongs to the **allocation**, not to whatever encloses it: the two spellings mean
    // different functions when the type has its own `operator new`, so a tree that dropped the qualification
    // would be lossless, well formed, diagnostic-free and wrong.
    let source = "void probe() { ::new (buf) Widget(); }";
    let allocation = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NewExpr)
        .expect("the allocation is a NewExpr");
    assert!(
        allocation
            .children_with_tokens()
            .filter_map(|child| child.into_token())
            .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::Scope),
        "the `::` is the NewExpr's own token"
    );
}

/// A construct that must contain a node of `kind`, with the statement kind it must have.
#[track_caller]
fn assert_fragment_contains(fragments: &[(&str, CppSyntaxKind)]) {    let missing: Vec<String> = fragments
        .iter()
        .filter(|(fragment, kind)| !contains(&format!("void probe() {{ {fragment} }}"), *kind))
        .map(|(fragment, kind)| format!("  {fragment}\n      no {kind:?} in it"))
        .collect();

    assert!(
        missing.is_empty(),
        "{} of {} fragments are missing a node:{}",
        missing.len(),
        fragments.len(),
        missing
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
    );
}

/// A header name is **one token**, whatever the name is made of.
///
/// The bug this pins: the fold accepted `Identifier | Dot | Slash | Minus | Plus | IntegerLiteral` between the
/// delimiters, and `c++config` lexes as `c`, `++`, `config` — so `#include <bits/c++config.h>` did not fold, and
/// that is the most common include in libstdc++: every standard header writes it.
///
/// What it cost was not a worse tree but a **wrong target**. The analysis layer reconstructs the name from the
/// tokens when the fold declines, and its reconstruction started at the `<`, so the target came out
/// `<bits/c++config.h` and resolved to nothing. A file with an unresolved include is deliberately never cached
/// (`index::store`), so every one of those headers was re-parsed on every session and every declaration in it
/// was invisible — 54 of the 169 files in a measured `<vector>` closure. Both halves are pinned:
/// `cpp_code_analysis/tests/preprocess.rs` has the reconstruction, and this has the fold.
///
/// A whitelist here is a rule about the *lexer's token kinds* pretending to be a rule about header names, which
/// is why the fix is a list of what cannot appear rather than of what can. See `docs/grammar-gaps.md` entry 22.
#[test]
fn a_header_name_is_one_token_whatever_is_in_it() {
    let folds = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        tree.get_red_root()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::HeaderName)
    };

    assert!(folds("#include <vector>\n"), "the ordinary case");
    assert!(folds("#include \"local.h\"\n"), "and the quoted one");
    assert!(
        folds("#include <bits/c++config.h>\n"),
        "`++` splits the name into three tokens, and a header name is not limited to the tokens a whitelist \
         happened to list"
    );
    assert!(folds("#include <sys/a-b.h>\n"), "`-` is a token of its own too");
    assert!(folds("#include <a/b/c.hpp>\n"));

    // What really ends a header name — and the assertion that the widening did not swallow the line.
    assert!(
        !folds("#include <unterminated\nint x;\n"),
        "a newline ends the directive, so there is no header name to fold"
    );
    assert!(
        !folds("a < b;\n"),
        "and outside a directive `<` is still less-than, which is why this is a parser rule and not a lexer one"
    );
}

/// The node kinds the C++23 and C++20 constructs must produce, where the *statement* kind says nothing.
///
/// A parameter, a namespace and a `this` are not statements, so [`assert_statement_kind`] cannot reach them —
/// and the two that matter most here are exactly the ones a wrong reading would keep well formed:
///
/// * an **explicit object parameter** was silently read as *nothing*, with the tokens after the `this` forming a
///   phantom second member. The parameter count is the assertion that catches it;
/// * an **alternative spelling** must be the operator rather than a name, and the difference between `a and b`
///   and `a android` is one the shape has to see.
#[test]
fn modern_constructs_produce_the_right_nodes() {
    assert_fragment_contains(&[
        ("void f(this S& self);", CppSyntaxKind::Parameter),
        ("void f(this S& self);", CppSyntaxKind::ThisExpr),
        ("void f(this auto&& self);", CppSyntaxKind::Parameter),
        // The statement and the expression form, in the two positions they are written.
        ("throw 1;", CppSyntaxKind::ThrowStat),
        ("x = throw 1;", CppSyntaxKind::ThrowExpr),
        ("auto y = cond ? throw 1 : 2;", CppSyntaxKind::ThrowExpr),
        // An inline namespace is a namespace, not a declaration of something called `namespace`.
        ("inline namespace v1 { }", CppSyntaxKind::NamespaceDecl),
        // `concept` and `requires`: the declaration, the clause and the expression are three different nodes,
        // and the *clause* is the one a wrong reading keeps well formed. `RequiresExpr` is a primary expression,
        // so the three places it can be written are three different rules reaching the same node.
        (
            "template <typename T> concept C = true;",
            CppSyntaxKind::ConceptDecl,
        ),
        (
            "template <typename T> requires C<T> void f(T t);",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> void f(T t) requires C<T> { }",
            CppSyntaxKind::RequiresClause,
        ),
        // The clause after a trailing return type, and one after a template head on a class. A clause after a
        // *class head* is deliberately not read: the grammar gives it none, and reading one used to hand the
        // class body to the statement rule — see `concepts.rs`.
        (
            "template <typename T> auto g(T t) -> int requires C<T>;",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> requires C<T> struct S { };",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> concept C = requires(T t) { t.f(); };",
            CppSyntaxKind::RequiresExpr,
        ),
        ("auto r = requires { g(); };", CppSyntaxKind::RequiresExpr),
        (
            "auto r = requires(T t) { t.f(); };",
            CppSyntaxKind::Requirement,
        ),
        // A braced-init-list in *expression* position, which is an `InitListExpr` and not the `Initializer` a
        // declaration produces. `x = {1};` was a silent wrong tree until the empty-declarator defect was fixed,
        // and the two nodes are how a consumer tells "a declaration initialised with braces" from "an assignment
        // of a braced-init-list".
        ("x = {1};", CppSyntaxKind::InitListExpr),
        ("v.push_back({1, 2});", CppSyntaxKind::InitListExpr),
        ("int x = {1};", CppSyntaxKind::Initializer),
        // An **unnamed parameter** is a parameter, and the count is the assertion: the variable reading produced
        // zero of them and a well-formed declaration instead. See `direct_init.rs`.
        ("void f(T);", CppSyntaxKind::Parameter),
        (
            "template <typename T> void f(T) { }",
            CppSyntaxKind::ParameterList,
        ),
        (
            "template <typename T> void f(T) { }",
            CppSyntaxKind::CompoundStat,
        ),
        // The two constraint words as ordinary names: a call is a call and an assignment is an expression, with no
        // construct in either. A keyword token made all four of these unreadable. See `concepts.rs`.
        ("void f() { requires(); }", CppSyntaxKind::CallExpr),
        ("void f() { concept(); }", CppSyntaxKind::CallExpr),
        ("void f() { concept = 2; }", CppSyntaxKind::ExpressionStat),
        ("void f() { requires = 1; }", CppSyntaxKind::ExpressionStat),
        // A C-style cast to a name the file never declares — the reading the `)` decides. See `operators.rs`.
        ("auto d = (MyType)1.5;", CppSyntaxKind::CastExpr),
        ("auto d = (size_t)size;", CppSyntaxKind::CastExpr),
        (
            "buf = (char *)malloc((size_t)size + 1);",
            CppSyntaxKind::CastExpr,
        ),
        // …and the shapes that must *not* become casts, because they are valid expressions.
        ("auto d = (a) - b;", CppSyntaxKind::ParenExpr),
        ("auto d = (a)(b);", CppSyntaxKind::CallExpr),
        // `<` decided by what follows the list: an operand means it was a comparison, and the shape says so —
        // `x = a < b > c` is `x = ((a < b) > c)`, so the *absence* of a `TemplateArgumentList` is the assertion
        // and it lives in `operators.rs` (this helper asserts presence, not absence). What is pinned here is the
        // other side: a genuine template-id keeps its argument list.
        ("void f() { x = a < b > c; }", CppSyntaxKind::BinaryExpr),
        (
            "void f() { auto n = A<B>::value; }",
            CppSyntaxKind::TemplateArgumentList,
        ),
        // Adjacent string literals are one literal, so the initialiser holds one node. See `expressions.rs`.
        ("auto s = \"a\" \"b\" \"c\";", CppSyntaxKind::LiteralExpr),
        // A `for` header's step is an expression, not a declaration that named nothing.
        (
            "void f() { for (;; i++, k++) { } }",
            CppSyntaxKind::ExpressionStat,
        ),
        // An elaborated type specifier reaches the *type* reading: a `TypeId` in the payload is the assertion, and
        // the expression reading could not produce one.
        ("auto n = sizeof(struct S);", CppSyntaxKind::TypeId),
        ("auto n = (struct S*)p;", CppSyntaxKind::CastExpr),
        // A qualified declarator name is a definition head: the parameter list is the assertion, because the wrong
        // reading made it an initializer (or nothing at all).
        ("static void A::f<int>(int);", CppSyntaxKind::ParameterList),
        (
            "static void Widget::draw(T) { }",
            CppSyntaxKind::ParameterList,
        ),
        (
            "static void Widget::draw(T) { }",
            CppSyntaxKind::CompoundStat,
        ),
        // A cv-qualifier after the type: the name is the declarator's, so there is an `InitDeclarator` and an
        // `ArrayType` — the wrong reading put the name in the type and made the declarator a structured binding of
        // nothing, silently.
        ("char const w[2];", CppSyntaxKind::InitDeclarator),
        ("char const w[2];", CppSyntaxKind::ArrayType),
        // An array type in a type-id, and an index that must not become one — the *absence* of an `ArrayType`
        // for `a[0]` is asserted in `expressions.rs`, since this helper asserts presence.
        ("auto n = sizeof(int[4]);", CppSyntaxKind::ArrayType),
        ("auto n = sizeof(a[0]);", CppSyntaxKind::IndexExpr),
        // A `new` reads its own bounds: the `ArrayType` is the new-declarator's, so the `TypeId` is bare.
        ("auto p = new int[4];", CppSyntaxKind::TypeId),
        ("auto p = new int[4];", CppSyntaxKind::ArrayType),
        // The implementation's spelling of `try`/`catch`. The `CatchStat` is the assertion rather than the
        // `TryStat`, because that is the pairing the wrong reading loses: as an `InitListExpr` of an expression
        // statement there was no handler at all, and the brace matching of the whole function was off by one.
        (
            "void f() { __try { g(); } __catch (...) { h(); } }",
            CppSyntaxKind::CatchStat,
        ),
        // `throw(…)` is a **suffix of the declarator**, so the payload is a type — and reading it as a
        // throw-expression is the reading the tokens allow, which is why the `TypeId` is what is asserted.
        ("void f() throw(std::exception&);", CppSyntaxKind::TypeId),
        // A macro between `if` and its condition: the condition keeps its parentheses, so there is a `ParenExpr`.
        // The wrong reading takes the macro for a call and leaves an `ArgumentList` in its place — the two are
        // well formed either way, and only the node says which happened.
        ("void f() { if _GLIBCXX17_CONSTEXPR (x) { } }", CppSyntaxKind::ParenExpr),
        ("void f() { if (x) [[likely]] { } }", CppSyntaxKind::AttributeList),
        // The compiler's own keywords, and the assertion is the **parameter list**: the wrong reading made
        // `__cdecl` the declarator's name and `g(void)` a macro suffix, so there was no parameter list anywhere —
        // well formed, lossless, no diagnostic, and every name in it wrong.
        ("int __cdecl g(void);", CppSyntaxKind::ParameterList),
        ("int *__cdecl _errno(void);", CppSyntaxKind::ParameterList),
        (
            "__forceinline size_t f(const char * _src) { return 0; }",
            CppSyntaxKind::ParameterList,
        ),
        // `__restrict` is the qualifier the kind table names and nothing produced.
        ("int f(const char * __restrict__ _Src);", CppSyntaxKind::RestrictQual),
        // …and a keyword that *means* a standard specifier produces that specifier's node, so a consumer does not
        // have to know which compiler's spelling the file used.
        (
            "__forceinline int f(void) { return 0; }",
            CppSyntaxKind::InlineSpec,
        ),
        // The **array** is the assertion, twice over. In the first it is a *type argument* — `_Tp[_Nm]` is the
        // idiomatic array specialization, and reading it as a subscript would give the argument no type at all.
        // What tells the two apart is that `_Tp` is a **template parameter**, which is why the second is here:
        // `a` is not one, so `a[I]` stays an expression and no `ArrayType` may appear.
        (
            "template <typename _Tp, int _Nm> constexpr bool d<_Tp[_Nm]> = true;",
            CppSyntaxKind::ArrayType,
        ),
        (
            "template <typename T> void f(T[4]);",
            CppSyntaxKind::ArrayType,
        ),
    ]);

    // Exactly one parameter: the silent version produced zero here and a phantom member beside it.
    let source = "struct Probe { void f(this S& self); };";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Parameter)
            .count(),
        1,
        "an explicit object parameter is one parameter"
    );

    // And exactly one clause: a clause read as ending at its first term leaves the rest of the constraint to be
    // read by whatever follows, which is well formed and wrong. See `concepts.rs`.
    let source = "template <typename T> requires C<T> && C2<T> void f(T t);";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::RequiresClause)
            .count(),
        1,
        "a conjunction of two concepts is one clause"
    );

    // A K&R definition: the second list is the assertion, because the *first* one is what the parenthesis says
    // either way — `argc` is a good parameter type, so a modern parameter list reads the same tokens. The
    // declarations after the parenthesis are what says they were names. See `old_style.rs` for the full shape.
    let source = "int main(argc, argv)\nint argc;\nchar *argv[];\n{ return argc; }";
    assert!(
        contains(source, CppSyntaxKind::OldStyleParameterList),
        "the parameters declared after the parenthesis get a list of their own"
    );
    assert!(
        contains(source, CppSyntaxKind::CompoundStat),
        "and the body is still the definition's"
    );

    // A base clause with a **nested template-id**: the `BaseSpecifier` is the assertion, because the failure was
    // that the head was not recognised as one at all — the angle scan read the `>>` as a single closer's worth of
    // depth, the `{` of the body arrived with the depth still positive, and the class definition failed against
    // its own name. A tree without the base clause is lossless, well formed and one diagnostic away from right.
    let source = "class D : public Base<K, std::shared_ptr<V>> { };";
    assert!(
        contains(source, CppSyntaxKind::BaseSpecifier),
        "the nested base clause is read as a base clause"
    );
    // And both nested lists are lists: the empty `std::less<>` inside a parameter list is two argument lists, not
    // one that swallowed the other's closer.
    let source = "void f(const std::map<int, int, std::less<>> &m);";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
            .count(),
        2,
        "two template argument lists, one of them empty"
    );

    // A **pointer to member**: the `C::*` is the declarator's, and the type is only `int`. Read the other way —
    // `C::` joining the type — the tree was lossless, well formed, free of diagnostics and *wrong*: the
    // declarator sat inside the type node, so a consumer asking the declaration for its type got `int C::` and
    // one asking for its declarator got nothing. The text of the specifier sequence is what tells the two apart,
    // and it is the only assertion here that sees it.
    let source = "int C::*p;";
    let specifiers = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DeclSpecifierSeq)
        .expect("a specifier sequence")
        .text()
        .to_string();
    assert_eq!(
        specifiers, "int ",
        "the member-pointer operator belongs to the declarator, not to the type"
    );

    // A **macro invocation used as a definition**: the block is the declaration's *body*, not a statement that
    // follows it, and the statement inside it is read as a statement. Both halves matter — the shape is what the
    // reader of a test file needs (the body of a `TEST(...)` is where the code is), and the body being attached is
    // what `TEST(A, B) { int x = 1; }` failing used to cost.
    let source = "TEST(FormatPerformance, 1k_row) { int x = 1; }";
    let parsed = CppParser::parse(source, ParserConfig::default());
    let body = parsed
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .expect("the block is in the tree");
    assert_eq!(
        body.parent()
            .map(|parent| CppSyntaxKind::from(parent.kind())),
        Some(CppSyntaxKind::Declaration),
        "the block is the definition's body, not a statement after it"
    );
    assert_eq!(
        body.text().to_string(),
        "{ int x = 1; }",
        "and its own declarations are inside it"
    );

    // The **named** spelling of the same shape, asked of the tree because the typed layer's `get_name_text`
    // answers with the class's own name — the class is what the declaration is *about* — and the question here is
    // whether the declarator exists at all. The defect was that it did not: the name had joined the type.
    let source = "static const struct { int a; } priority[] = { { 1 } };";
    let named = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
        .flat_map(|declarator| declarator.descendants())
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
        .map(|name| name.text().to_string())
        .any(|name| name == "priority");
    assert!(
        named,
        "the declarator is named `priority`, not part of the type"
    );

    // The `:` of a range-`for` inside a **member function** is the range separator, and the assertion is the
    // statement kind — the wrong reading produced a well-formed bit-field declaration whose width was the range
    // expression, with no diagnostic at all in the version that had a `,` after it.
    let source = "struct S { void f() { for (auto &v: vec) { } } };";
    assert!(
        contains(source, CppSyntaxKind::RangeForStat),
        "a range-for inside a member function is a range-for, not a bit-field"
    );
    // …and the same for a label, which is the other statement whose first token a declaration reading can take.
    let source = "struct S { void f() { again: g(); } };";
    assert!(
        contains(source, CppSyntaxKind::LabelStat),
        "a label inside a member function is a label"
    );
    // The bit-field itself is unchanged, at the member level it belongs to.
    let source = "struct S { int bits : 3; };";
    assert!(
        contains(source, CppSyntaxKind::Initializer),
        "a member's `:` is still a bit-field width"
    );

    // **How many enumerators an enum has.** This is the assertion the A0 defect of B26 needed and did not have:    // with the initializer read at the comma-operator level, `enum E { A = 0, B };` came out as *one*
    // `EnumeratorDecl` whose initializer was the expression `0, B` — lossless, well formed, no diagnostic, and
    // with `B` no longer a member of the enum. Counting members is the only question that sees it, and the census
    // above cannot ask it ("does this construct parse?" was answered `yes` by the wrong tree all along).
    for (source, expected) in [
        ("enum E { A = 0, B };", 2),
        ("enum E { A = 0, B, };", 2),
        ("enum class E : unsigned { A = 1, B = 2, C };", 3),
        ("enum E { A, B, C };", 3),
        ("enum E { A = 0 };", 1),
    ] {
        assert_eq!(
            CppParser::parse(source, ParserConfig::default())
                .get_red_root()
                .descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::EnumeratorDecl)
                .count(),
            expected,
            "{source:?} has {expected} enumerators"
        );
    }
}

#[test]
fn a_macro_whose_own_body_opens_a_namespace_is_its_own_statement() {
    // `bits/c++config.h` writes `inline _GLIBCXX_BEGIN_NAMESPACE_VERSION` and, twenty lines later,
    // `_GLIBCXX_END_NAMESPACE_VERSION`, and defines both of them **in that file** as `namespace __8 {` and `}`.
    // Nothing among the file's own tokens says a namespace opened or a brace closed, so the declarations that
    // followed were read as the continuation of a declaration that never ends. The reading has to come from the
    // macro's own `#define` body, which the directive rule records as token kinds (B87).
    let source = "#define BEGIN_N namespace __8 {\n#define END_N }\ninline BEGIN_N\nint x;\nEND_N\n";
    let root = CppParser::parse(source, ParserConfig::default()).get_red_root();

    assert_eq!(
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode)
            .count(),
        0,
        "an invocation whose own body opens the namespace is a statement of its own"
    );
    assert_eq!(
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count(),
        2,
        "`inline BEGIN_N` and `END_N` are invocations, not a declaration and an expression"
    );

    // A macro whose body *begins* with another token is not a namespace head: `_GLIBCXX_MATH_NS` is `__8`, and
    // reading it as one would take the declarator rules' job away.
    let source = "#define NS __8\ninline NS n;\n";
    let root = CppParser::parse(source, ParserConfig::default()).get_red_root();
    assert_eq!(
        root.descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count(),
        0,
        "a macro that names a namespace rather than opening one is not a head"
    );
}

#[test]
fn a_template_parameter_may_default_to_a_conditional_expression() {
    // `bits/ranges_util.h:265-267`:
    //
    // ```cpp
    // template<input_or_output_iterator _It, sentinel_for<_It> _Sent = _It,
    //          subrange_kind _Kind = sized_sentinel_for<_Sent, _It>
    //            ? subrange_kind::sized : subrange_kind::unsized>
    // ```
    //
    // The default is a **conditional expression whose head is a type**. The type reading takes
    // `sized_sentinel_for<_Sent, _It>` and stops, and the `?` was then left to the parameter list, which reported
    // ``expected `,` or `>` `` against it. A type is only the answer when the list can continue after it.
    let source = "enum class subrange_kind { sized, unsized };\n\
                  template<typename A, typename B> struct sized_sentinel_for { };\n\
                  template<typename _It, typename _Sent = _It,\n\
                           subrange_kind _Kind = sized_sentinel_for<_Sent, _It>\n\
                             ? subrange_kind::sized : subrange_kind::unsized>\n\
                  struct subrange { };\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a conditional default reads as a value, not as a type that stops at the `?`"
    );

    // The type reading still wins where it **is** the whole default: `std::vector<int>` is a type-id and also a
    // perfectly good expression, and reading it as one would lose the argument list.
    let source = "template<typename T> struct V { };\n\
                  template<typename T, V<T> Arg = V<T>()> struct W { };\n";
    assert!(
        reads(source, Where::File).is_ok(),
        "a type-id default keeps its reading"
    );
}

#[test]
fn a_directive_may_stand_on_either_side_of_a_declarations_initializer() {
    // `ext/concurrence.h:58` writes one variable's value **once per branch**:
    //
    // ```cpp
    //   _GLIBCXX17_INLINE const _Lock_policy __default_lock_policy =
    //   #ifndef __GTHREADS
    //     _S_single;
    //   #elif defined _GLIBCXX_HAVE_ATOMIC_LOCK_POLICY
    //     _S_atomic;
    //   #else
    //     _S_mutex;
    //   #endif
    // ```
    //
    // The `#` therefore stands where the *initializer* should begin, which no expression rule can read: the
    // declaration failed and the file reported `expected primary expression` against its own `#ifndef`. The seam
    // is the one the enumerator list, the template parameter list and the parameter list already have (B23), in
    // the one position that was missing it.
    let source = "const int x =\n#ifndef Y\n  1;\n#else\n  2;\n#endif\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a directive between the `=` and the value is a seam, not an initializer"
    );
    assert!(
        contains(source, CppSyntaxKind::Initializer),
        "and the value after the directive is still that declarator's initializer"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::PreprocessorDirective),
        3,
        "all three directives stay nodes rather than becoming rubble"
    );

    // **The other side of the same seam** — `bits/stl_algobase.h:906` — because a conditional whose branches are
    // values leaves the `#endif` between the value and the declaration's own `;`:
    //
    // ```cpp
    //   const bool __load_outside_loop =
    //   #if __has_builtin(__is_trivially_constructible) \
    //         && __has_builtin(__is_trivially_assignable)
    //       __is_trivially_constructible(_Tp, const _Tp&)
    //       && __is_trivially_assignable(__decltype(*__first), const _Tp&)
    //   #else
    //       __is_trivially_copyable(_Tp)
    //   #endif
    //       ;
    // ```
    //
    // The `\`-continued condition is part of the control: it is the same directive, and a seam that only reached
    // the end of a line would report against the second `__has_builtin`.
    let source = "template<typename _Tp> void f() {\n\
                    bool b =\n\
                  #if __has_builtin(__is_trivially_constructible) \\\n\
                        && __has_builtin(__is_trivially_assignable)\n\
                      1\n\
                  #endif\n\
                      ;\n\
                    (void)b;\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a directive between the value and the `;` is the same seam"
    );

    // **The seam is not a licence to swallow a directive anywhere.** A declaration with no initializer still has
    // no initializer: `int x` followed by `#if` and a value is `int x;`-shaped only if the file says so, and the
    // reading here is the one the grammar had before — the value is not adopted as `x`'s initializer.
    assert!(
        !contains("int x\n#if 1\n  1\n#endif\n;\n", CppSyntaxKind::Initializer),
        "a directive after a declarator does not create an initializer"
    );
}

#[test]
fn the_attribute_spelling_with_one_trailing_underscore_is_an_attribute() {
    // `parallel/compatibility.h:48-49` — libstdc++'s own header, which writes the GNU spelling **short**:
    //
    // ```cpp
    // extern "C"
    // __attribute((dllimport)) void __attribute__((stdcall)) Sleep (unsigned long);
    // ```
    //
    // `g++ -std=c++17` accepts that line, so `__attribute` is the compiler's extension and not this parser's
    // guess. Read as a name it is a macro-shaped specifier, and the declaration it stands in front of was lost:
    // the file reported ``expected `;` `` against its own `extern "C"`.
    let source = "extern \"C\"\n__attribute((dllimport)) void __attribute__((stdcall)) Sleep (unsigned long);\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "both attribute spellings read, so the declaration they modify is read"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        2,
        "each of the two is one attribute list, whichever spelling it uses"
    );

    // The spelling is the whole test, so the control is the *shape*: a name followed by a `(` where a declarator
    // has already ended is not an attribute, and `int __attribute = 1;` is a variable named by that name.
    assert_eq!(
        count_of("int __attribute = 1;\n", CppSyntaxKind::AttributeList),
        0,
        "`__attribute` with no parenthesis after it is a name, not an attribute"
    );
    assert_eq!(
        reads("int __attribute = 1;\n", Where::File),
        Ok(()),
        "…and the declaration it names still reads"
    );
}

#[test]
fn an_enumerator_may_carry_a_macro_before_its_value() {
    // `omp.h:74` writes an enumerator whose attribute is a macro, between the name and the `=`:
    //
    // ```c
    //   omp_proc_bind_master __GOMP_DEPRECATED_5_1
    //     = omp_proc_bind_primary,
    // ```
    //
    // The `[[…]]` spelling of the same position was already read; the macro is the same seam one spelling along,
    // and read as a name the enumerator ended at it — the loop then wanted a `,` or a `}` where the `=` stood, and
    // the whole `typedef enum … } omp_proc_bind_t;` came out as rubble from there on.
    let source = "#define DEP __attribute__((__deprecated__))\n\
                  typedef enum E {\n\
                    A DEP\n\
                      = 1,\n\
                    B = 2\n\
                  } E;\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a macro between an enumerator's name and its `=` is a seam, not the end of the enumerator"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::EnumeratorDecl),
        2,
        "and both enumerators are still enumerators"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        1,
        "the macro is read as the invocation it is"
    );

    // The control is the **same seam with the compiler's own spelling**, which has to keep working: the attribute
    // is not a macro and is not counted as one.
    let source = "enum E { A [[deprecated]] = 1, B = 2 };\n";
    assert_eq!(reads(source, Where::File), Ok(()), "`[[…]]` still reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        0,
        "an attribute is not a macro invocation"
    );
}

#[test]
fn a_template_argument_may_be_a_comparison_in_parentheses() {
    // `tuple:2439` — `__enable_if_t<(__i >= sizeof...(_Types))>` — where the *type* reading of the argument
    // consumed `(__i`, gave up at `>=`, and was then accepted as a complete argument: the `>=` was split into `>`
    // and `=`, the `>` closed the list, and the declaration had no head left. A failed reading is not an answer.
    let source = "template<bool B, typename T = void> struct C { };\n\
                  using size_t = unsigned long long;\n\
                  template<size_t __i, typename... _Types>\n\
                    C<(__i >= sizeof...(_Types))>\n\
                    f(const C<true>&);\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the argument is the parenthesised comparison, not the two tokens the failed reading stopped between"
    );

    // `type_traits:987` — `struct __is_signed_helper<_Tp, true> : public __bool_constant<_Tp(-1) < _Tp(0)>` — is
    // the other half of the same rule: three scans count angle brackets, and all three counted the `<` **inside
    // the group** as opening a level, so the list's own `>` was one level short of the end and the answer was
    // "this `<` is a less-than". g++ reads all three of these as arguments.
    let source = "template<bool B> struct C { };\n\
                  template<typename T> struct S : public C<T(0) < T(0)> { };\n";
    assert_eq!(reads(source, Where::File), Ok(()), "a comparison in a base clause reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::BaseSpecifier),
        1,
        "and the base clause is a base clause"
    );

    // The spelled-out forms, which are the ones a reader can check at a glance — the first three are arguments
    // (g++ accepts them), the fourth is an inner list and must keep meaning one.
    for argument in [
        "1 < 2",
        "sizeof(int) < 3",
        "(T(0) < T(0))",
        "::std::vector<int>",
    ] {
        let source = format!(
            "template<bool B> struct C {{ }};\n\
             template<typename T> struct S : public C<{argument}> {{ }};\n"
        );
        assert_eq!(
            reads(&source, Where::File),
            Ok(()),
            "`{argument}` is one argument"
        );
    }
}

#[test]
fn a_condition_declaration_ends_at_the_paren_that_closes_the_condition() {
    // `tr1/riemann_zeta.tcc:173` — `if (_GLIBCXX_MATH_NS::fmod(__s,_Tp(2)) == _Tp(0))` — where the condition's
    // declaration reading *succeeded* and took the call with it: `NS::fmod` is a qualified name and therefore a
    // type, `(s, T(2))` is then a function declarator, and both of the rule's own tests passed on it — a name was
    // parsed (the parameters' own names) and something was initialised (`T(2)` is a parameter with a
    // direct-initialiser). The condition then wanted its `)` where the `==` stood.
    //
    // A condition's declaration has no `;` of its own, so the `)` is what says it is over, and requiring it is
    // the third refusal — a shape that is not a condition declaration is an expression.
    let source = "namespace NS { template<typename T> T fmod(T a, T b); }\n\
                  template<typename T> T f(T s) {\n\
                    if (NS::fmod(s,T(2)) == T(0))\n\
                      return T(0);\n\
                    return T(1);\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the call the file wrote is read as a call"
    );
    assert!(
        contains(source, CppSyntaxKind::CallExpr),
        "and the call is in the tree"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::Initializer),
        0,
        "the condition has no initializer — the misreading was a *parameter* with a direct-initialiser, and that \
         is the node it produced"
    );

    // The control is every condition that **is** a declaration: one variable, initialised, and the `)` right
    // after it. Each of these has to keep the declaration reading — the rule exists for them.
    for condition in [
        "Foo* p = get()",
        "const auto n = g()",
        "Foo p = get()",
    ] {
        let source = format!(
            "struct Foo {{ }};\n\
             Foo* get();\n\
             void h() {{ if ({condition}) {{ }} }}\n"
        );
        assert_eq!(
            reads(&source, Where::File),
            Ok(()),
            "`if ({condition})` still reads"
        );
        assert_eq!(
            count_of(&source, CppSyntaxKind::Initializer),
            1,
            "`if ({condition})` declares its variable, and that is the one initializer in the file"
        );
    }
}

#[test]
fn an_attribute_may_stand_between_the_star_and_the_name() {
    // `lwpintrin.h:43` and the whole `*intrin.h` family:
    //
    // ```cpp
    // extern __inline void * __attribute__((__gnu_inline__, __always_inline__, __artificial__))
    // __slwpcb (void) { … }
    // ```
    //
    // The position is the one `eat_cv_qualifiers` owns — "the same rule as the specifier position, one token
    // later" — and an attribute is one of the things written there. Left out, this was a **silent wrong tree**
    // rather than a diagnostic: the declarator was *named* `__attribute__` and initialised with `((…))`, and the
    // name the file wrote stood after it as a macro. With a body the second initializer made it loud.
    let source = "extern void * __attribute__((__always_inline__)) __slwpcb (void) { }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the declaration reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        1,
        "the attribute is an attribute list, in its own node"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        1,
        "and `(void)` is the declarator's parameter list, which is what makes this a function"
    );

    // The same shape **without** a body is where the silent half was: a declarator named `__attribute__`
    // initialised with `(x)` and `f` left over. Zero initializers is the claim.
    let source = "void * __attribute__((x)) f;\n";
    assert_eq!(reads(source, Where::File), Ok(()), "`f` reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::Initializer),
        0,
        "nothing here is an initializer — the attribute belongs to the pointer"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        0,
        "and the name is the declarator's, not a macro standing after one"
    );

    // The control is the qualifier that already stood there: `* const` is not an attribute, and must not be read
    // as one.
    let source = "void * const f (void) { }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "`* const` still reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        0,
        "`const` is a qualifier, not an attribute"
    );
}

#[test]
fn a_conditional_may_be_written_with_a_directive_before_its_colon() {
    // `type_traits:2269-2275` — the conditional's `:` branch is written **once per branch**, with the directive
    // between the `?` branch and the `:`. A `#` where the `:` should be is not a `:`, so the directive is read as
    // its own node and the `:` is asked for again; before this the file reported ``expected :, but get #`` against
    // the `# if` itself.
    let source = "int f(int n) {\n\
                    return n > 2\n\
                      ? 3\n\
                  #if 1\n\
                      : 4\n\
                  #endif\n\
                      ;\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a directive between the two branches of a conditional is a seam"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::TernaryExpr),
        1,
        "the conditional is still one conditional"
    );

    // **A directive before the `?` also reads**, by a *different* seam than the one above: the expression's own.
    // The operator loop meets the `#if`, sees that no *operator* follows it (a `?` is not one — the conditional is
    // read a level up), and takes the directives with it before stopping. That is the same reading that makes a
    // *term* of an expression repeatable per branch, and it was measured on the same corpus.
    assert_eq!(
        reads(
            "int f(int n) {\n  return n\n#if 1\n    ? 1\n#endif\n    : 2;\n}\n",
            Where::File
        ),
        Ok(()),
        "a directive before the `?` is read by the expression's own seam"
    );

    // …and the expression's seam **leaves an `#else` alone**, because that one opens another instance of the
    // construct *around* the expression: `bits/stl_algobase.h:906` is a declaration whose initializer is written
    // once per branch, and the initializer's own seam can only see the `#else` if the expression gave it back.
    // Both values being in the tree is the claim.
    let source = "int x =\n#if 1\n  1\n#else\n  2\n#endif\n  ;\n";
    assert_eq!(reads(source, Where::File), Ok(()), "a value per branch reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::LiteralExpr),
        2,
        "both branches' values are in the initializer — the expression handed the `#else` back"
    );
}

#[test]
fn a_macro_invocation_at_the_head_of_a_declaration_is_a_specifier() {
    // `backward/binders.h:133-137` — an attribute macro written **with its argument list**, before the type:
    //
    // ```cpp
    //   template<typename _Operation, typename _Tp>
    //     _GLIBCXX11_DEPRECATED_SUGGEST("std::bind")
    //     inline binder1st<_Operation>
    //     bind1st(const _Operation& __fn, const _Tp& __x)
    // ```
    //
    // The macro is not in any table the parser is usually handed — `binders.h` contains no `#include` at all, so
    // read on its own every table is empty — and the *body* test beside this rule cannot claim it either: a
    // function-like macro's body holds its own parameter (`__attribute__((__deprecated__(ALT)))`), not a specifier
    // list. What is left is the position: nothing but a specifier may stand at the head of a declaration.
    let source = "template<typename T> struct B { };\n\
                  template<typename T>\n\
                    DEP_SUGGEST(\"std::bind\")\n\
                    inline B<T>\n\
                  f(const T& x)\n\
                  { return B<T>(); }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "an attribute macro with an argument list reads as a specifier, not as a name with a stray group"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        1,
        "and the invocation is one node, with its arguments kept as tokens"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ArgumentList),
        1,
        "the group after the name is the macro's argument list"
    );

    // **The control is the follower.** `CHECK(1);` in a body is a *call*: the group closes and the declaration is
    // already over, so the specifier arm must not claim it — claiming it would give a declaration with no
    // declarator, which is well formed, lossless and silent, and would put the call the user wrote nowhere in the
    // tree. The declaration reading is still tried first (a `;` after a group is a plain function declaration in
    // C++), and it fails here because `1` is not a parameter — so the statement is the call it looks like.
    let source = "int main() { CHECK(1); return 0; }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "`CHECK(1);` still reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        0,
        "a call whose group ends the statement is not a macro invocation standing for a specifier"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::CallExpr),
        1,
        "it is the call the expression rules own"
    );
}

#[test]
fn a_term_of_an_expression_may_be_written_once_per_branch() {
    // `tr1/riemann_zeta.tcc:179` — the terms of a product repeat per branch, each branch writing the **same
    // operator** with a different operand, and the expression continues after the `#endif`:
    //
    // ```cpp
    //   __zeta *= std::pow(…) * std::sin(…)
    // #if _GLIBCXX_USE_C99_MATH_TR1
    //          * std::exp(_GLIBCXX_MATH_NS::lgamma(_Tp(1) - __s))
    // #else
    //          * std::exp(__log_gamma(_Tp(1) - __s))
    // #endif
    //          / __numeric_constants<_Tp>::__pi();
    // ```
    //
    // A `#` in operator position cannot be anything else, so the directive is read as its own node and the loop
    // looks for the operator again. The tokens of both branches end up in one chain — the branches are
    // alternatives and one expression cannot say so — which is the documented trade (B104/B108).
    let source = "double f(double x) {\n\
                    double z = 1;\n\
                    z *= pow(x)\n\
                  #if 1\n\
                       * exp(x)\n\
                  #else\n\
                       * exp(-x)\n\
                  #endif\n\
                       / pi();\n\
                    return z;\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a directive where the next operator goes is a seam"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::BinaryExpr),
        4,
        "the chain is folded as one expression: `*=`, and the `*`, `*`, `/` of its right side"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::PreprocessorDirective),
        3,
        "and all three directives are in the tree rather than swallowed"
    );

    // **…and a `#endif` inside the parentheses** is the same seam at the end of the expression rather than in the
    // middle of it: `bits/stl_algobase.h:1237` writes two independent conditionals, each contributing an operand.
    let source = "bool f(bool a, bool b) {\n\
                    return ((a\n\
                  #if 1\n\
                          || b\n\
                  #endif\n\
                  #if 1\n\
                          || true\n\
                  #endif\n\
                         ) && a);\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the closers are read before the `)` the caller expects"
    );
}

#[test]
fn a_failed_cast_is_a_parenthesised_expression() {
    // The rule exists twice in the expression grammar — the unary rule and the primary one — and they disagreed
    // about the one thing that matters. `(V<T>(x))` is a *functional conversion inside a grouped expression*: the
    // type reading takes `V<T>(x)` for a function type, the `)` matches, and then **no operand follows**. The
    // primary rule rewinds and reads the group as the expression it is; the copy in the unary rule reported a
    // missing operand instead, so the same tokens parsed or failed depending on which copy saw them first.
    // `tr1/bessel_function.tcc` and the whole `_Tp(2)` family are that shape.
    let source = "template<typename T> struct V { };\n\
                  template<typename T> T g(T);\n\
                  template<typename T> bool f(T x) { bool b = (V<T>(g(x))); return b; }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a cast with no operand is a parenthesised expression"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ParenExpr),
        1,
        "and it is read as one"
    );

    // The control is the cast that **does** hold up, which has to keep its reading.
    let source = "template<typename T> bool f(T x) { bool b = (bool)(x); return b; }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "a real cast reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::CastExpr),
        1,
        "and it is a cast"
    );
}

#[test]
fn an_attribute_may_stand_between_a_declarator_ids_operators() {
    // `bits/stl_function.h:1398` and `tuple:2532` — an attribute between the declarator's id and its
    // **parameters**, which is where the standard puts it and where the file's own header writes it:
    //
    // ```cpp
    // bool operator== [[nodiscard]] (const tuple<_Tps...>& __t, const tuple<_Ups...>& __u) { … }
    // ```
    //
    // Read as "an attribute, then a `(`" only: the parameter list that follows is still this declarator's suffix,
    // so it stays inside the `Declarator` where every consumer looks for it.
    let source = "bool operator== [[nodiscard]] (int a, int b) { return a == b; }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the shape reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        1,
        "the attribute is an attribute"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        1,
        "and the `(int, int)` is the declarator's parameter list"
    );

    // The **other** position is the control: an attribute after the parameters belongs to the declaration, and
    // the parameter list is already there.
    let source = "int x [[maybe_unused]] = 1;\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the other position reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        1,
        "still one attribute"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        0,
        "and no parameter list was invented for a variable"
    );
}

#[test]
fn a_macro_with_a_block_may_be_a_whole_statement() {
    // `debug/safe_iterator.h:74-79` defines the two macros that bracket a scope, `[&]() -> void` in one branch and
    // **nothing at all** in the other, and uses them around a block inside an `if`:
    //
    // ```cpp
    // 	if (…) SCOPE_BEGIN { __gnu_cxx::__scoped_lock __l(this->_M_get_mutex()); … } SCOPE_END
    // 	else
    // ```
    //
    // A name followed by a `{` is otherwise C++11's list-initialisation (`Point{1, 2};`), which is what the
    // postfix rule read — and then every statement in the block was inside a braced list. Evidence is what tells
    // them apart, and the tables have to say the name is a macro.
    let source = "#define SCOPE_BEGIN\n\
                  #define SCOPE_END\n\
                  int g();\n\
                  void f(int a) {\n\
                    if (a)\n\
                      SCOPE_BEGIN {\n\
                        int x = g();\n\
                      } SCOPE_END\n\
                    else\n\
                      { }\n\
                  }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the block belongs to the invocation, and the `else` still finds its `if`"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        2,
        "both invocations are invocations"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::CompoundStat),
        3,
        "the function body, the block the opening macro stands in front of, and the `else` branch"
    );

    // …and the **closing** macro on its own, inside a body, with no `;` of its own: a bare name reads as a macro
    // statement only with evidence and only where a statement can end.
    let source = "#define END\nvoid f() { END }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "a bare invocation reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        1,
        "as one invocation"
    );

    // The control: a name nobody described is **not** a macro, so `Vec{1, 2};` keeps the reading C++ gives it.
    let source = "struct Vec { };\nvoid f() { Vec{1, 2}; }\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a declared type's braced value reads"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        0,
        "and it is not an invocation"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::InitListExpr),
        1,
        "it is the braced list it looks like"
    );
}

#[test]
fn a_conditional_inside_template_arguments_keeps_its_colon() {
    // `bits/ranges_util.h:436` — a deduction guide whose return type has a **conditional expression** as a
    // template argument:
    //
    // ```cpp
    //   template<borrowed_range _Rng>
    //     subrange(_Rng&&)
    //       -> subrange<iterator_t<_Rng>, sentinel_t<_Rng>,
    // 		  (sized_range<_Rng> || sized_sentinel_for<…>)
    // 		  ? subrange_kind::sized : subrange_kind::unsized>;
    // ```
    //
    // The scan that decides "is this `<` a template argument list" stops at a `:`, because a `:` cannot stand
    // inside one — except as the middle of a conditional. So it ended the scan, the `<` was read as a less-than,
    // and the deduction guide stopped at its own name: ``expected `;` `` against the `<` of the return type.
    // Counting the `?` is the fix, and the count is what keeps the stop set's `:` for every other shape.
    let source = "enum class K { sized, unsized };\n\
                  template<typename A, typename B, K k> struct sub { };\n\
                  template<typename T> struct it { };\n\
                  using A = it<int>;\n\
                  using B = it<long>;\n\
                  auto f() -> sub<A, B, (true) ? K::sized : K::unsized>;\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a conditional is an argument like any other value"
    );

    // The control is the `:` the stop set is *for*: a base clause still ends the scan, so `struct S : B<C> { }`
    // reads as a base clause rather than as something else.
    let source = "template<bool B> struct C { };\n\
                  struct S : C<true> { };\n";
    assert_eq!(reads(source, Where::File), Ok(()), "a base clause reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::BaseSpecifier),
        1,
        "and it is a base specifier"
    );
}

#[test]
fn a_declarations_tail_may_be_written_once_per_branch() {
    // **The three spellings of one shape**, each in the file the census found it in: a construct's *tail* — its
    // last element, the token that closes it, and the `;` that ends the declaration — written once per branch,
    // with the head shared. Every one of them used to read as far as the first branch's `;` and then fail on the
    // second branch's text, because the declaration was already finished.
    //
    // ```cpp
    //     random_shuffle(_RAIter, _RAIter,                       // parallel/algorithmfwd.h:700 — a parameter list
    // #if __cplusplus >= 201103L
    // 		   _RandomNumberGenerator&&);
    // #else
    // 		   _RandomNumberGenerator&);
    // #endif
    //
    //   using __iter_key_t = remove_const_t<                     // bits/stl_iterator.h:3090 — an argument list
    // #ifdef __glibcxx_tuple_like
    //       tuple_element_t<0, typename iterator_traits<_It>::value_type>>;
    // #else
    //       typename iterator_traits<_It>::value_type::first_type>;
    // #endif
    //
    //     return __len > (…) ? _Max_align::value                 // type_traits:2269 — a conditional's `:` branch
    // # if _GLIBCXX_USE_BUILTIN_TRAIT(__builtin_clzg)
    // 	     : 1 << (__SIZE_WIDTH__ - __builtin_clzg(__len - 1u));
    // # else
    // 	     : 1 << (__LLONG_WIDTH__ - __builtin_clzll(__len - 1ull));
    // # endif
    // ```
    //
    // The rule is the same in all three: the `)`/`>` in this branch closes the construct *in this spelling*, the
    // `;` after it is that branch's, and when the `;` is followed by an `#else`/`#elif` **and** a conditional was
    // opened after the construct began, the next branch's tail is read as the same construct. The declaration is
    // told the terminator came from a branch, because it would otherwise ask for a `;` and find `#endif`.
    let cases = [
        "void random_shuffle(_RAIter, _RAIter,\n#if X\n  _RandomNumberGenerator&&);\n#else\n  \
         _RandomNumberGenerator&);\n#endif",
        "using K = remove_const_t<\n#if X\n  T<0, I>>;\n#else\n  I::first_type>;\n#endif",
        "int f(int n) {\n  return n > 2\n    ? 3\n#if X\n    : 1 << (n - 1);\n#else\n    : 1 << (n - 2);\n#endif\n}",
    ];
    for source in cases {
        assert_eq!(
            reads(source, Where::File),
            Ok(()),
            "the tail written once per branch reads: {source:?}"
        );
    }

    // **One construct, not one per branch.** A second `ParameterList`/`TemplateArgumentList`/`TernaryExpr` is the
    // silent wrong tree this seam could produce, so the counts are the assertion.
    assert_eq!(
        count_of(cases[0], CppSyntaxKind::ParameterList),
        1,
        "one parameter list, with both branches' spellings of the tail in it"
    );
    assert_eq!(
        count_of(cases[0], CppSyntaxKind::Parameter),
        4,
        "and four parameters: `_RAIter` twice, then both spellings of the last one"
    );
    assert_eq!(
        count_of(cases[1], CppSyntaxKind::TemplateArgumentList),
        2,
        "one argument list for the alias and one for the `T<0, I>` inside it"
    );
    assert_eq!(
        count_of(cases[2], CppSyntaxKind::TernaryExpr),
        1,
        "one conditional, with both spellings of its `:` branch"
    );

    // …and the **terminator shared** spelling of the same shape, which is what a *definition* with a per-branch
    // parameter list looks like (`bits/stl_algo.h:4600`): no `;` in either branch, and the body after the `#endif`
    // — so nothing about the terminator is the list's, and the declaration still owns its `{`.
    let source = "void random_shuffle(int __first, int __last,\n#if X\n  long&& __rand)\n#else\n  long& __rand)\n\
                  #endif\n{ }";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a per-branch parameter list whose terminator is shared reads"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        1,
        "one list, with both branches' spellings in it"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::CompoundStat),
        1,
        "and the body after the `#endif` is still the function's body"
    );

    // The control is a **declaration** written per branch, which looks the same from the `;` and must not be read
    // as a per-branch tail: the `#if` there belongs to the statement, not to the list.
    let source = "#if X\nvoid f(int a);\n#else\nvoid g(long a);\n#endif";
    assert_eq!(reads(source, Where::File), Ok(()), "a declaration per branch reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::ParameterList),
        2,
        "with one parameter list in each branch"
    );
}

#[test]
fn an_implementation_keyword_may_stand_where_an_operand_goes() {
    // `bits/uniform_int_dist.h:318` — `__extension__` *inside* an expression, which is where GCC's own headers
    // write it when they use an extension on purpose:
    //
    // ```cpp
    // 		__ret = __extension__ _S_nd<unsigned __int128>(__urng, __u64erange);
    // ```
    //
    // The word is the implementation's own, so reading it as one costs no ordinary name — while the operand rule
    // would otherwise take it for the operand's *name* and leave the real operand as two expressions in a row.
    let source = "template<typename T> int nd(T a, T b);\n\
                  void f(unsigned long a, unsigned long b, int& r) {\n\
                    r = __extension__ nd<unsigned long>(a, b);\n\
                  }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the operand reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::CallExpr),
        1,
        "and the call after the keyword is one call"
    );

    // The control is an ordinary name in the same position, which keeps its reading as the operand.
    let source = "int g(int);\nvoid f(int a) { int r = g(a); }\n";
    assert_eq!(reads(source, Where::File), Ok(()), "an ordinary call reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::CallExpr),
        1,
        "as one call"
    );
}

#[test]
fn a_qualified_head_is_a_type_even_when_it_ends_in_template_arguments() {
    // `bits/ranges_util.h:483` — a **variable template's partial specialisation**, whose name is qualified and
    // ends in an argument list:
    //
    // ```cpp
    //   template<typename _Iter, typename _Sent, subrange_kind _Kind>
    //     inline constexpr bool __detail::__is_subrange<subrange<_Iter, _Sent, _Kind>> = true;
    // ```
    //
    // The specifier sequence reads the whole qualified name as the *type* — which is what a qualified name in type
    // position is, the head of a definition — so nothing is left to name the declarator, and the guard that
    // refuses an initializer without a name fired. It asks the *recorded* qualified flag first, and for this
    // spelling that record is empty: the name ends in a template argument list, so the walk that recovers a name
    // stops on the `>`. The token-level walk is the one that answers.
    let source = "enum class K { a };\n\
                  template<typename T, typename U, K k> struct sub { };\n\
                  template<typename I, typename S, K k>\n\
                    inline constexpr bool n::is_sub<sub<I, S, k>> = true;\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the qualified head is the definition's name, and `= true` initialises it"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::Declaration),
        3,
        "the enum, the primary template and the specialization"
    );

    // The control is the shape the guard exists for: an assignment with no declaration's type in front of it is
    // an expression, and a bare name is not a type.
    assert_eq!(
        reads("void f(int n) { x = 1; }\n", Where::File),
        Ok(()),
        "an assignment reads"
    );
    assert_eq!(
        count_of("void f(int n) { x = 1; }\n", CppSyntaxKind::Initializer),
        0,
        "and it is not a declaration with an initializer"
    );
}

#[test]
fn a_run_of_annotations_may_stand_in_front_of_the_declaration_it_annotates() {
    // MSVC's SAL annotations are a **run** of reserved names before the declaration, and `<sal.h>` gives them
    // bodies that are themselves macro calls. `ucrt/stdio.h:100-102`:
    //
    // ```c
    // _Check_return_wat_
    // _Success_(return == 0)
    // _ACRTIMP errno_t __cdecl fopen_s(…);
    // ```
    //
    // The declaration reading fails on the *second* name's group — `_Check_return_wat_ _Success_(return == 0)`
    // reads as `type name(parameters)` and `_ACRTIMP` cannot continue it — and the statement then came out an
    // `ExpressionStat` that wanted a `;` at the name it could not consume. Measured on the closure: that was the
    // first error of **fourteen** UCRT headers (B133).
    let source = "_Check_return_wat_\n_Success_(return == 0)\n_ACRTIMP errno_t __cdecl fopen_s(FILE** _Stream);\n";

    // **Both channels, because the reading has to be clean in both.** With no toolchain behind it a parse has
    // never heard of these names (the census's `--seeds` run without a closure), and with the closure's bodies in
    // force `_Success_` is a macro with a positional body — measured, and the two were *different* failures before
    // this rule: the first shape errored on the declaration, the second on the very same `;`.
    //
    // What the two do **not** share is how much of the run is an invocation, and that difference is the evidence:
    // a name nobody knows is read as a specifier of the declaration that follows, while a name with a body is its
    // own invocation. Both are clean, both are lossless, and only the second one puts every annotation where it
    // belongs — which is what the closure's bodies are for.
    assert_eq!(
        shape_body_of_the_file_reads(source, None),
        vec!["MacroCall:_Check_return_wat_", "Declaration:_Success_"],
        "with no evidence, the run's remaining names are specifiers of the declaration"
    );

    let closure = MacroEnvironment::from_included_macros([
        IncludedMacro::defined_with_body(
            0,
            "_Check_return_wat_",
            false,
            MacroBody::Unknown,
            Some("_Check_return_"),
        ),
        IncludedMacro::defined_with_body(
            0,
            "_Success_",
            true,
            MacroBody::Unknown,
            Some("_SAL2_Source_(_Success_, (expr), _Success_impl_(expr))"),
        ),
    ]);
    assert_eq!(
        shape_body_of_the_file_reads(source, Some(&closure)),
        vec![
            "MacroCall:_Check_return_wat_",
            "MacroCall:_Success_",
            "Declaration:_ACRTIMP",
        ],
        "and with the closure's bodies, every annotation is its own invocation"
    );

    // The control is the run with **no group anywhere**, and it is here to say what this rule does *not* do: two
    // reserved names in front of a declaration are already read as two invocations by the rule for a macro standing
    // where a declaration goes, and this rule leaves that reading exactly as it was (measured with the change
    // reverted: the same three nodes, byte for byte). The `saw_a_group` gate is what keeps the two rules from
    // overlapping on a shape whose reading nobody asked to change.
    assert_eq!(
        shape_body_of_the_file_reads("_Check_return_ _Ret_notnull_ int f(void);\n", None),
        vec![
            "MacroCall:_Check_return_",
            "MacroCall:_Ret_notnull_",
            "Declaration:int",
        ],
        "a run with no group keeps the reading it already had"
    );

    // Two more negatives the rule is written to keep, measured rather than assumed: a run that ends at no
    // declaration at all never reaches this rule (the anchor refuses it), and the error a file already had stays
    // the error it had.
    assert_eq!(
        shape_body_of_the_file_reads("_Check_return_ _Success_(x)\n", None),
        vec!["MacroCall:_Check_return_", "MacroCall:_Success_"],
        "reserved names with a group and nothing after them are two invocations, not a run before a declaration"
    );
}

#[test]
fn annotations_may_stand_in_runs_before_the_type_they_annotate() {
    // MSVC's UCRT writes **runs** of SAL annotations where a parameter's specifiers go, and the type comes after
    // them (`ucrt/stdio.h:400-402`):
    //
    // ```c
    // _ACRTIMP void __cdecl setbuf(
    //     _Inout_                                             FILE* _Stream,
    //     _Inout_updates_opt_(BUFSIZ) _Post_readable_size_(0) char* _Buffer
    //     );
    // ```
    //
    // The arm that reads "a macro where a specifier goes" allowed exactly **one**, at the head of the sequence
    // (`specifiers == 0`), so the second annotation was read as a *type*, its group as that type's declarator, and
    // the declaration came out `expected primary expression` against its own first line. Which is what the closure
    // measured: `stdio.h`'s first error was `_ACRTIMP void __cdecl setbuf(`, and fourteen UCRT headers were behind
    // it (B134).
    for source in [
        // Two annotations, one group each — the shape the header writes.
        "int f(_Inout_updates_opt_(BUFSIZ) _Post_readable_size_(0) char* _Buffer);\n",
        // …and any number of them.
        "int f(Wat(1) Wobble(2) Qux(3) int x);\n",
        // The neighbour that already worked, kept so the relaxation cannot be what breaks it.
        "int f(Wat(1) char* x);\n",
        "int f(Wat(1) Wobble char* x);\n",
    ] {
        assert_eq!(reads(source, Where::File), Ok(()), "{source:?}");
    }

    // **The reading that must not change**, and the measurement that put the name follower out of the simple case
    // (`objbase.h:95`): a group followed by a name whose own group *is* the declarator's parameter list is a
    // declaration whose head is the invocation — one node, not two. The chained question is what tells the two
    // apart: `CoFreeLibrary (…)` is followed by `;`, so it is the head; a second annotation is followed by a
    // specifier, so it is not.
    assert_eq!(
        shape_body_of_the_file_reads("WINOLEAPI_(HINSTANCE) CoFreeLibrary (HINSTANCE hInst);\n", None),
        vec!["Declaration:WINOLEAPI_"],
        "a name after the group is the declaration's head, and its `(…)` is the parameter list"
    );

    // The **mirror image is still a gap**, and this assertion is its guard rather than its fix: a *bare* annotation
    // in front of a grouped one is read as a type by the same arm's precondition (the cursor must be on a name with
    // a group), which is the remaining half of this family in `ucrt/stdio.h:612`
    // (`_In_z_ _Printf_format_string_params_(2) char const* _Format`). It fails the day it starts working — see
    // `docs/grammar-gaps.md` B134.
    assert!(
        reads("int f(Wat Wobble(2) char const* _Format);\n", Where::File).is_err(),
        "a bare annotation before a grouped one is the next shape, not this one"
    );
}

#[test]
fn a_requires_expression_may_be_a_template_argument() {
    // `type_traits:3946` — a class head whose base clause carries a requires-expression as an argument, and the
    // `;` *inside* its body is what the scans had to learn about.
    //
    // ```cpp
    //   template<typename _Tp>
    //     struct is_scoped_enum<_Tp>
    //     : bool_constant<!requires(_Tp __t, void(*__f)(int)) { __f(__t); }>
    //     { };
    // ```
    //
    // Two scans read those tokens before any rule does: the one that decides whether a `<` opens an argument list,
    // and the one that decides whether a class head is followed by a body. Both stopped at a `;` — a structural
    // boundary, and it is, but only at depth zero: a `;` inside a matched `{ … }` is a statement in the body of a
    // lambda or of a requires-expression, and neither ends the declaration.
    let source = "template<bool B> struct bool_constant { };\n\
                  template<typename T> struct is_scoped_enum\n\
                    : bool_constant<!requires(T t, void(*f)(int)) { f(t); }> { };\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "the requires-expression is an argument, and the base clause reads"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::BaseSpecifier),
        1,
        "as a base specifier"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::RequiresExpr),
        1,
        "with the requires-expression in the tree"
    );

    // The control is the boundary the stop set is *for*: a `;` at depth zero still ends the scan, so a class head
    // with no body in front of it is not read as one.
    assert!(
        reads("template<bool B> struct C { };\nstruct S : C<true>;\n", Where::File).is_err(),
        "a class head with no body is still an error"
    );
}

#[test]
fn an_alias_may_carry_an_attribute_macro_before_its_equals() {
    // `type_traits:2825` and the rest of the library's deprecations, written with a macro rather than the
    // attribute keyword:
    //
    // ```cpp
    //   template<class _Tp, size_t _Len>
    //     using aligned_storage_t _GLIBCXX23_DEPRECATED
    //       = typename aligned_storage<_Len, _Alignof<_Tp>>::type;
    // ```
    //
    // The position is the one `[[deprecated]]` already had — between an alias's own name and its `=` — read with
    // the reader the declarator's suffixes use, which knows a macro invocation with arguments from one without.
    let source = "template<class T> struct storage { using type = T; };\n\
                  template<class T>\n\
                    using storage_t DEPRECATED23\n\
                      = typename storage<T>::type;\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the alias reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        1,
        "and the macro between the name and the `=` is read as an invocation"
    );

    // The control is the attribute spelling of the same position, which has to keep working and is not a macro.
    let source = "using T [[deprecated]] = int;\n";
    assert_eq!(reads(source, Where::File), Ok(()), "the attribute reads");
    assert_eq!(
        count_of(source, CppSyntaxKind::AttributeList),
        1,
        "it is an attribute list"
    );
    assert_eq!(
        count_of(source, CppSyntaxKind::MacroCall),
        0,
        "and not a macro invocation"
    );
}

#[test]
fn a_macro_may_supply_the_rest_of_a_template_parameter_list() {
    // `bits/refwrap.h:142` writes
    //
    // ```cpp
    // template<typename _Res, typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM>
    // ```
    //
    // and `_GLIBCXX_NOEXCEPT_PARM` is `, bool _NE` **in `bits/c++config.h`**: the list's separator and a whole
    // parameter live in a header, so nothing among this file's tokens says the list continues. What carries the
    // answer is the position — an identifier there can neither continue the parameter that was just read nor close
    // the list, so the file is already wrong unless the name is a macro standing where the separator goes (B88).
    let source = "template<typename _Res, typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM>\nstruct S;\n";
    assert_eq!(
        reads(source, Where::File),
        Ok(()),
        "a macro-shaped name may stand where the `,` of the list goes"
    );
    assert!(
        contains(source, CppSyntaxKind::MacroCall),
        "and it reads as an invocation, not as the parameter's name"
    );

    // The control has to be the **pack** form, which is the shape the real file writes: after `typename... _Args`
    // there is no room for another name, so the same position with an ordinary name is a mistake and only a name
    // written like a macro is rescued. (After a *non*-pack parameter the reader accepts a name there — `template
    // <typename T foo>` is clean today — so that shape cannot tell the two readings apart.)
    assert!(
        reads("template<typename... _Args foo> struct S;\n", Where::File).is_err(),
        "an ordinary name there is not a macro"
    );
    assert!(
        reads("template<typename... _Args int> struct S;\n", Where::File).is_err(),
        "a keyword there is not a macro either"
    );
}

#[test]
fn a_literal_operator_may_be_spelled_with_a_space_before_its_suffix() {
    // `literal-operator-id` has two productions — `operator "" identifier` and
    // `operator user-defined-string-literal` — and MSVC's `<xstring>` writes the spaced one, for every `sv` in the
    // file:
    //
    // ```cpp
    // _NODISCARD constexpr string_view operator"" sv(const char* _Str, size_t _Len) noexcept { … }
    // ```
    //
    // Which production was written decides how much of the name the **lexer** put in one token: written together,
    // `""_km` is a `UserDefinedLiteral` and the whole name is in it; written apart it is a `StringLiteral` and then
    // an identifier. Only the first was read after `operator`, so the suffix was left for the declarator — which
    // wanted `;` at `noexcept`, gave up, and left the file an `ExpressionStat` where a function definition was.
    // Measured: that line is `<xstring>`'s **first** error, with 98 more behind it, and `<xstring>` is where every
    // `std::string` fact comes from (B132).
    let spaced = "constexpr int operator\"\" _km(double) noexcept { return 0; }\n";
    assert_eq!(
        reads(spaced, Where::File),
        Ok(()),
        "the spaced spelling is the same name as the unspaced one"
    );

    // …and it is read as a **name**: the suffix belongs to the operator, not to whatever comes after it. Asserted
    // on the shape rather than on the error count because a tolerant reader can accept this construct in two ways
    // and only one of them puts `_km` where a declaration's name goes.
    let tree = CppParser::parse(spaced, ParserConfig::default());
    let named: Vec<String> = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
        .map(|node| node.text().to_string())
        .collect();
    assert!(
        named.iter().any(|name| name == "operator\"\" _km"),
        "the operator's name is `operator\"\" _km`, suffix and all: {named:?}"
    );
    assert!(
        named.iter().all(|name| !name.contains("noexcept")),
        "and nothing after the parameter list is inside it: {named:?}"
    );

    // The unspaced spelling is the one the lexer already made a single token of, and it must keep working.
    let unspaced = "constexpr int operator\"\"_km(double) noexcept { return 0; }\n";
    assert_eq!(reads(unspaced, Where::File), Ok(()));

    // A class member is the same name in the same position (this one was already clean, and it is the control that
    // says the fix is about the spelling rather than about literal operators in general).
    assert_eq!(
        reads("int operator\"\" _km(double) noexcept { return 0; }", Where::Class),
        Ok(())
    );
}

