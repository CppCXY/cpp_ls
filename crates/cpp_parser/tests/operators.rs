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
    // `(f)(x)` and `(T)(x)` are the same tokens, and the call reading is the one that keeps the arguments. A
    // cast to a bare undeclared name is therefore the documented trade-off, and this is the other side of it.
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
