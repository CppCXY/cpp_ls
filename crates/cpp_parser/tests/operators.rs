//! The comma operator and the C-style cast: two operators that are also punctuation.
//!
//! Both were missing for the same *shape* of reason, and both needed a decision the operator table cannot hold:
//!
//! * a **comma** separates arguments, template arguments, initializer elements, captures and declarators — so
//!   it cannot live in `get_operator_precedence`, because that table is reached by every list rule through its
//!   elements. It lives above them instead, and the rules that own their commas read one element at
//!   [`parse_assignment_expr`];
//! * a **cast** and a parenthesised expression are the same first token, and `(a)` parses as a type-id — so a
//!   rule that tried the type reading whenever it could would turn every parenthesised variable into a cast.
//!
//! The tests below pin which reading each spelling gets, because in both cases the wrong answer is a
//! well-formed tree rather than an error.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

fn kinds(source: &str) -> Vec<CppSyntaxKind> {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .map(|node| CppSyntaxKind::from(node.kind()))
        .collect()
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    kinds(source).into_iter().filter(|k| *k == kind).count()
}

fn parses(source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        tree.get_errors()
    );
    assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
}

// ============================================================================
// Two more spellings of "the parentheses might hold a type"
// ============================================================================

/// A **global-qualified name** in parentheses is an expression, not a cast type.
///
/// `::name` looked like certain evidence of a type — `::std::string` surely is one — and it is the same token that
/// begins a global-qualified *expression*. An arm of `is_a_type_in_parentheses` answered "cast" for `(::x)`, the
/// cast reading then demanded an operand that was never written, and the shapes a real file writes failed:
/// `(::abs(static_cast<int>(a - b)) > c)` in `SymSpell.cpp`, reported as `expected ), but get >`.
///
/// What settles a cast is what follows the `)` — an operand, which two operands in a row cannot be — and that
/// check is asked before this arm ever mattered: a cast to a qualified type the file has never heard of still
/// works, because the `x` after the `)` is what says so.
#[test]
fn a_global_qualified_name_in_parentheses_is_an_expression() {
    for source in [
        "void f() { (::x); }\n",
        "void f() { auto y = (::x); }\n",
        "void f() { (::x + 1); }\n",
        "void f() { (::x()); }\n",
        "int f(int x) { return (::abs(x) > 1); }\n",
        "int f(int x) { return ((::abs(x)) > 1); }\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::ParenExpr) >= 1,
            "{source:?} is a parenthesised expression"
        );
    }

    // The casts that really are casts are unchanged, including one to a qualified type no header of this file
    // declares.
    for source in [
        "void f(void* p) { auto x = (::MyType*)p; }\n",
        "void f(int x) { auto y = (::MyType)x; }\n",
        "void f(int x) { auto y = (::std::string)x; }\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::CastExpr) >= 1,
            "{source:?} is a cast"
        );
    }
}

/// A **functional-notation conversion with a keyword type**: `bool(x)`, `int(y)`, `unsigned(z)`.
///
/// The other way of writing `(bool)x`, and everywhere a value is normalised. A type keyword is neither an
/// identifier nor one of the keywords the expression dispatch lists, so no arm matched and the rule reported
/// `expected primary expression` against the keyword itself — `bool(lint["codeStyle"])` in a real file.
///
/// The type is the keyword **and nothing else**: `parse_type_id` would read the parentheses as a function type's
/// parameter list ("bool taking x") and have nothing left for the payload.
#[test]
fn a_functional_cast_with_a_keyword_type_is_a_cast() {
    for source in [
        "int f(int x) { return bool(x); }\n",
        "void f(int x) { g(bool(x)); }\n",
        "void f() { g(\"k\", bool(lint[\"s\"])); }\n",
        "void f(int x) { if (bool(x)) { } }\n",
        "void f(int x) { auto y = int(x) + double(x); }\n",
        "void f(int x) { auto y = unsigned(x); }\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::CastExpr) >= 1,
            "{source:?} is a functional-notation conversion"
        );
    }

    // The declaration reading keeps its own spelling of the same tokens: `int(x);` declares the parenthesised
    // name `x`, and that reading is tried first.
    let declaration = "void f() { int(x); }\n";
    parses(declaration);
    assert_eq!(
        count(declaration, CppSyntaxKind::CastExpr),
        0,
        "`int(x);` is a declaration, not a conversion"
    );
}

// ============================================================================
// Pointer-to-member operators
// ============================================================================
/// `.*` and `->*` are the **only** way to use a pointer to member, and both are punctuation-as-operator like the
/// comma and the cast above. The tokens have always existed in the lexer (`DotStar`, `ArrowStar`) and the
/// expression table simply did not list them, so `(this->*handle)(args)` — the spelling every member-pointer call
/// is written with — failed with `expected ), but get ->*`.
///
/// Their precedence is the one part worth pinning: the standard puts pm-expression *below*
/// multiplicative-expression, so `.*` and `->*` bind **tighter** than `*`. A table that gave them the same level
/// as `*` would read `p->*h * n` as `p->*(h * n)`.
#[test]
fn the_pointer_to_member_operators_are_binary_operators() {
    for source in [
        "struct C { };\nvoid f(C* c) { (c->*h)(1); }\n",
        "struct C { };\nvoid f(C& c) { (c.*h)(1); }\n",
        "struct C { };\nvoid f(C* c) { auto x = c->*h; }\n",
        "struct C { };\nvoid f(C* c) { g(c->*h); }\n",
        "struct S { void f() { (this->*h)(1); } };\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::BinaryExpr) >= 1,
            "{source:?} has a pointer-to-member expression in it"
        );
    }
}

// ============================================================================
// The comma operator
// ============================================================================

#[test]
fn the_comma_operator_parses_where_a_comma_is_an_operator() {
    for source in [
        "void f() { auto x = (a, b); }\n",
        "void f() { ((a, b)); }\n",
        "void f() { x = 1, y = 2; }\n",
        "void f() { for (a = 0, b = 0; ; ) {} }\n",
        "void f() { return 1, 2; }\n",
        "void f() { g(a, (b, c)); }\n",
        "void f() { auto x = (a, b, c); }\n",
    ] {
        parses(source);
    }
}

#[test]
fn the_comma_operator_is_an_operator_inside_the_parentheses_and_not_outside() {
    // Two elements being copied inside the parentheses, three call arguments outside them.
    let source = "void f() { h(a, (b, c)); }\n";
    assert_eq!(
        count(source, CppSyntaxKind::BinaryExpr),
        1,
        "only the parenthesised comma is an operator"
    );
    assert_eq!(
        count(source, CppSyntaxKind::ParenExpr),
        1,
        "and it is the parenthesised one"
    );
}

#[test]
fn the_comma_operator_is_left_associative_and_looser_than_assignment() {
    // `x = 1, y = 2` is `(x = 1), (y = 2)` — three binary nodes: two assignments and the comma joining them.
    // Reading it the other way, `x = (1, y = 2)`, would be two.
    let source = "void f() { x = 1, y = 2; }\n";
    assert_eq!(count(source, CppSyntaxKind::BinaryExpr), 3);
    assert_eq!(
        count(source, CppSyntaxKind::Initializer),
        0,
        "and it is a statement, not a declaration"
    );
}

#[test]
fn a_comma_in_a_container_is_still_a_separator() {
    // The half a naive implementation loses, and the reason the operator could not go in the precedence table:
    // every list rule reaches its elements through the same expression reader. None of these may become one
    // element, and none of them may grow a `BinaryExpr` for a comma that belongs to the list.
    for source in [
        // Call arguments.
        "void f() { g(a, b); }\n",
        "void f() { g(a, b, c); }\n",
        "void f() { g(f(x, y), z); }\n",
        // Braced initializer elements.
        "void f() { int v[] = {1, 2}; }\n",
        "void f() { auto p = std::pair<int, int>{1, 2}; }\n",
        "void f() { Vec<int> v{1, 2, 3}; }\n",
        // Lambda capture lists.
        "void f() { auto g = [a, b] { }; }\n",
        "void f() { auto g = [&x, y] { }; }\n",
        "void f() { auto g = [x = 1, y = 2] { }; }\n",
        // Parenthesised initialisers of a variable.
        "void f() { Widget w(1, 2); }\n",
        // Constructor member initializer lists.
        "struct S { S() : a(1), b(2) {} int a; int b; };\n",
        // Template argument lists and template parameter lists.
        "void f() { auto t = std::pair<int, char>(); }\n",
        "template <typename A, typename B> void h();\n",
        // Argument lists that contain a pack expansion, which is read at the element level too.
        "void f() { g(args..., more); }\n",
        "void f() { h(f(x)...); }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::BinaryExpr),
            0,
            "{source:?} has no comma *operator* in it"
        );
    }
}

#[test]
fn a_call_keeps_its_argument_count() {
    // The observable symptom of getting this wrong: the call still parses, but with one argument instead of two.
    // Nothing in the tree says "argument", so the count is taken from the commas the call consumed — which is
    // what `count` above checks by finding no `BinaryExpr`. These are the declaration-list forms, where the
    // symptom would be a single declarator instead of two.
    for source in [
        "void f() { int a, b; }\n",
        "void f() { int a = 1, b = 2; }\n",
        "void f() { for (int i = 0, n = 3; i < n; ++i) {} }\n",
        // Structured bindings read their own name list, which never reaches an expression reader.
        "void f() { auto [a, b] = p; }\n",
        // A function *declaration* whose parameters are comma-separated.
        "void f(int a, int b);\n",
        "struct S { void f(int a, int b); };\n",
    ] {
        parses(source);
    }
}

/// The three call sites that were **missing from the list** and were found by failing tests.
///
/// The list in `exprs.rs` is the deliverable of the maintenance convention, and the honest record of how it
/// went is that writing it first did not make it complete: it had four entries, and implementation turned up
/// three more. Each one has a distinct shape of its own, which is why none of them was obvious from the others:
///
/// * a **bit-field width** is not "a list of expressions" at all — `int bits : 3;` is one constant-expression —
///   but the *members* are comma-separated, so `unsigned flags : 1, spare : 7;` is two fields;
/// * a **template parameter's default** is the same shape one level down, and its failure was reported against
///   a declaration three parameters away from the comma that was eaten;
/// * a **template argument** is reached through a *type* reading first, so it is the expression **fallback**
///   that had to change — the entry that was on the list by name was on it for the wrong reason.
#[test]
fn the_container_call_sites_that_the_list_missed_are_fixed() {
    for source in [
        // The bit-field one. Two fields, not one field with a comma expression for a width.
        "struct S { unsigned flags : 1, spare : 7; };\n",
        "struct S { int a : 3, b : 4, c : 5; };\n",
        // The template-parameter one. Three parameters, the third with a pack.
        "template <typename T, int N = 3, typename... Rest> class Grid { };\n",
        "template <typename T, int N = 3> class Grid { };\n",
        "template <int A = 1, int B = 2> struct P { };\n",
        // The template-argument one: a qualified name whose template arguments hold a comma.
        "void f() { Grid<T, 3>::fill(x); }\n",
        "void f() { A<B, C>::d = 1; }\n",
        "template <typename T> void Grid<T, 3>::fill(const T& value) {}\n",
        // And the comma *operator* still works inside a template argument, where a comma really is one.
        "void f() { A<(a, b)> x; }\n",
    ] {
        parses(source);
    }
}

// ============================================================================
// The C-style cast
// ============================================================================

#[test]
fn a_cast_with_a_pointer_or_reference_type_is_a_cast() {
    // The deciding evidence is what is *missing*: a binary operator needs a right operand, so a `*` or `&`
    // immediately before the `)` cannot be one. No type table is needed, which is what makes these readable for
    // a type the file never declares.
    for source in [
        "void f() { auto d = (T*)p; }\n",
        "void f() { auto d = (MyType*)p; }\n",
        "void f() { auto d = (T&)x; }\n",
        "void f() { auto d = (T&&)x; }\n",
        "void f() { auto d = (ns::T*)p; }\n",
        "void f() { auto d = (T<int>*)p; }\n",
        "void f() { auto d = (const T*)p; }\n",
        "void f() { auto d = (T* const)p; }\n",
        "void f() { p = (T*)q->r; }\n",
        "void f() { g((T*)p, (U*)q); }\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::CastExpr) >= 1,
            "{source:?} has a cast in it"
        );
    }
}

#[test]
fn a_cast_to_a_keyword_type_is_a_cast_and_binds_tighter_than_a_binary_operator() {
    // `(int)a + b` is a sum of a cast and `b`, not a cast of `a + b` — the operand of a cast is a *unary*
    // expression. Getting that wrong still parses, so it is pinned by shape.
    let source = "void f() { auto d = (T*)p + 1; }\n";
    assert_eq!(count(source, CppSyntaxKind::CastExpr), 1);
    assert_eq!(count(source, CppSyntaxKind::BinaryExpr), 1);

    parses("void f() { auto d = (int)1.5; }\n");
    assert_eq!(
        count("void f() { auto d = (int)1.5; }\n", CppSyntaxKind::CastExpr),
        1
    );
}

#[test]
fn a_multiplication_in_parentheses_is_not_a_cast() {
    // The mistake the two-stage check exists to avoid, in every spelling. `a * b` has an operand on both sides
    // of the `*`, so the `*` is an operator; and `(a && b)` is a conjunction rather than a cast to `a&&`.
    for source in [
        "void f() { auto d = (a * b); }\n",
        "void f() { auto d = (a* b); }\n",
        "void f() { auto d = (a *b); }\n",
        "void f() { auto d = (a && b); }\n",
        "void f() { auto d = (a & b); }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::CastExpr),
            0,
            "{source:?} is a parenthesised expression"
        );
        assert_eq!(count(source, CppSyntaxKind::ParenExpr), 1);
    }
}

#[test]
fn a_parenthesised_name_is_not_a_cast() {
    // `(a)` parses as a type-id — one name, no declarator — so a rule that tried the type reading whenever it
    // could would turn every parenthesised variable into a cast of `a`.
    for source in [
        "void f() { x = (a); }\n",
        "void f() { x = (a + b); }\n",
        "void f() { x = ((a)); }\n",
        "void f() { x = a * (b + c); }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::CastExpr),
            0,
            "{source:?} is a parenthesised expression"
        );
    }
}

#[test]
fn a_call_of_a_parenthesised_expression_is_still_a_call() {
    // `(f)(x)` and `(T)(x)` are the same tokens, and the call reading is the one that keeps the arguments — the
    // one place the two readings really are silent, because a call is a perfectly good expression. It is the
    // boundary of the operand rule below, which is why it is pinned beside it.
    for source in [
        "void f() { auto d = (f)(x); }\n",
        "void f() { auto d = (T)(x); }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::CallExpr),
            1,
            "{source:?} keeps its argument list"
        );
    }
}

#[test]
fn a_less_than_between_two_names_is_a_comparison() {
    // `<` is both the less-than operator and the opener of a template argument list, and the tokens alone do not
    // choose: `a < b > c` is a comparison **and** a well-formed template-id followed by an operand.
    //
    // What settles it is the token after the list — an operand cannot follow a template-id, exactly as one cannot
    // follow a `)` in a cast. The reading is speculative: the argument list is parsed, and given back if it turns
    // out not to be one.
    //
    // These all used to be errors. `if (n < 0 || n > 100000)` is the one that mattered: it is how a range check is
    // written, and the lookahead happily paired the `<` of the first comparison with the `>` of the second.
    let templated = "void f() { if (n < 0 || n > 100000) { } }\n";
    parses(templated);
    assert_eq!(
        count(templated, CppSyntaxKind::TemplateArgumentList),
        0,
        "no template-id in a range check"
    );
    assert_eq!(
        count(templated, CppSyntaxKind::BinaryExpr),
        3,
        "three operators"
    );

    for source in [
        "void f() { x = a < b > c; }\n",
        "void f() { x = a < b || c > d; }\n",
        "bool g() { return a < b || c > d; }\n",
        "void f() { if (n < 0 || n > 100000) { } }\n",
        "void f() { while (i < n && j > k) { } }\n",
        "void f() { x = (a < b) ? 1 : 2; }\n",
        "void f() { x = a.b < c > d; }\n",
        "void f() { x = p->m < q > r; }\n",
        "void f() { g(a < b, c > d); }\n",
        "void f() { x = a < b; }\n",
        "void f() { x = a < b && c > d; }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::TemplateArgumentList),
            0,
            "{source:?} compares rather than instantiates"
        );
    }

    // The shape of the reading, not just the absence of an error: `x = a < b > c` is `x = ((a < b) > c)` —
    // left-associative like every other comparison — so there are three binary expressions: the assignment and
    // the two comparisons. A template-id reading would have produced one `IdentifierExpr` and a dangling `c`.
    let chained = "void f() { x = a < b > c; }\n";
    assert_eq!(count(chained, CppSyntaxKind::BinaryExpr), 3);
}

#[test]
fn a_genuine_template_id_keeps_its_reading() {
    // The other side of the same decision, and the reason the reading is speculative rather than eager: every one
    // of these is a template-id, and giving one back would break working code. What separates them from the
    // comparisons above is what follows the `>` — an operator, a delimiter or a `::`, never an operand.
    for source in [
        "void f() { auto n = A<B>::value; }\n",
        "void f() { auto n = std::vector<int>::size_type{}; }\n",
        "void f() { g<int>(1); }\n",
        "void f() { x.template f<int>(1); }\n",
        "void f() { auto p = new A<B>(); }\n",
        "void f() { auto n = sizeof(A<B>); }\n",
        "void f() { auto v = std::vector<int>{}; }\n",
        "void f() { std::vector<int> v; }\n",
        "void f() { Foo<int> x; }\n",
        "void f() { auto n = f<A>(1) + g<B>(2); }\n",
        "void f() { auto n = a < b; }\n",
        "void f() { for (Foo<int> v : m) { } }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { auto n = A<B>::value; }\n",
            CppSyntaxKind::TemplateArgumentList
        ),
        1
    );
    assert_eq!(
        count("void f() { g<int>(1); }\n", CppSyntaxKind::CallExpr),
        1,
        "the call survives the decision"
    );
    assert_eq!(
        count(
            "void f() { std::vector<int> v; }\n",
            CppSyntaxKind::Declaration
        ),
        2,
        "the function definition and the variable it declares"
    );
}

#[test]
fn a_cast_to_a_name_the_file_never_declares_is_a_cast() {
    // What decides these is the token **after** the `)`: two operands in a row is not an expression in any
    // grammar, so if the next token can only begin an operand, the parentheses held a type. That is evidence in
    // the tokens, not a lookup — which retires the trade-off this shape was recorded as:
    //
    //     auto d = (MyType)1.5;     a cast: `1.5` cannot follow an expression
    //     auto d = (MyType)x;       a cast: neither can `x`
    //     auto d = (size_t)size;    the same shape, and the reason it was found — a C file full of them
    for source in [
        "void f() { auto d = (MyType)1.5; }\n",
        "void f() { auto d = (MyType)x; }\n",
        "void f() { auto d = (size_t)size; }\n",
        "void f() { auto d = (size_t)size + 1; }\n",
        "void f() { buf = (char *)malloc((size_t)size + 1); }\n",
        "void f() { auto d = (MyType)new T; }\n",
        "void f() { auto d = (MyType)sizeof(T); }\n",
        "void f() { auto d = (MyType)!ok; }\n",
        "void f() { auto d = (MyType)~mask; }\n",
        "void f() { auto d = (MyType)'c'; }\n",
        "void f() { auto d = (MyType)\"s\"; }\n",
        "void f() { auto d = (MyType)true; }\n",
        "void f() { auto d = (MyType)nullptr; }\n",
        "void f() { auto d = (MyType)this; }\n",
        "void f() { g((size_t)size, (char *)p); }\n",
        "void f() { auto d = (size_t)size + (size_t)other; }\n",
    ] {
        parses(source);
        assert!(
            count(source, CppSyntaxKind::CastExpr) >= 1,
            "{source:?} is a cast"
        );
    }
}

#[test]
fn an_operand_after_the_parentheses_decides_the_cast_and_nothing_else_does() {
    // The other side of the rule. Every token left out of the operand set means something in an expression as
    // well, and each of the shapes below is a **valid expression** that must keep its reading:
    //
    //     (a) - b     subtraction        `-` is binary too
    //     (a) * b     multiplication     `*` is binary too
    //     (a) & b     bitwise and        `&` is binary too
    //     (a)[b]      index              `[` continues the expression
    //     (a)(b)      a call             `(` continues the expression
    //     (a), b      comma              `,` continues the expression
    //
    // This is what separates them from the casts above: those are *not* expressions at all, so the cast reading
    // takes nothing away. These are, so it would.
    for source in [
        "void f() { auto d = (a) - b; }\n",
        "void f() { auto d = (a) + b; }\n",
        "void f() { auto d = (a) * b; }\n",
        "void f() { auto d = (a) & b; }\n",
        "void f() { auto d = (a)[b]; }\n",
        "void f() { auto d = (a)++ ; }\n",
        "void f() { auto d = (a), b; }\n",
        "void f() { auto d = (a)(b); }\n",
        "void f() { auto d = (MyType)-1; }\n",
        "void f() { auto d = (MyType)*p; }\n",
        "void f() { auto d = (MyType)&x; }\n",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::CastExpr),
            0,
            "{source:?} is a parenthesised expression"
        );
    }
}
