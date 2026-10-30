//! **C++11 syntax**, as a project that adopted it would write it.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("a lambda", "void f() { auto g = [](int n) { return n; }; }"),
    (
        "a lambda with a capture",
        "void f(int x) { auto g = [x](int n) { return x + n; }; }",
    ),
    ("auto", "void f() { auto n = 1; }"),
    (
        "a range-based for",
        "void f(int* values) { for (int x : values) { (void)x; } }",
    ),
    ("nullptr", "void f() { int* p = nullptr; }"),
    (
        "override and final",
        "struct B { virtual void f(); }; struct D : B { void f() override; }; struct E final : B { };",
    ),
    ("an rvalue reference", "void f(int&& n);"),
    (
        "a variadic template",
        "template <class... Ts> void f(Ts... values);",
    ),
    ("decltype", "void f() { decltype(1) n = 1; }"),
    ("an alias template", "template <class T> using Alias = T*;"),
    (
        "uniform initialisation",
        "struct S { int a; int b; }; void f() { S s{1, 2}; }",
    ),
    ("constexpr", "constexpr int f() { return 1; }"),
    ("static_assert", "static_assert(sizeof(int) == 4, \"int\");"),
    ("an enum class", "enum class Colour { Red, Green };"),
    (
        "a delegating constructor",
        "struct S { S() : S(1) { } S(int n); };",
    ),
    (
        "a user-defined literal",
        "constexpr int operator\"\"_km(unsigned long long n) { return (int)n; }",
    ),
    ("= delete", "struct S { S(const S&) = delete; };"),
    ("a raw string", "const char* s = R\"(a \" b)\";"),
    ("[[noreturn]]", "[[noreturn]] void f();"),
    // **`typeid` carries a postfix chain**: `typeid(*p).name()` asks the type for a property, and the `.` belongs to
    // the typeid-expression. Without the suffixes the node was complete at its `)` and the member access had no rule.
    (
        "typeid with a postfix member access",
        "void f() { auto p = g(); std::cout << typeid(*p).name(); }",
    ),
    (
        "typeid of a type with a postfix member access",
        "void f() { std::cout << typeid(int).name(); }",
    ),
];

#[test]
fn every_cpp11_construct_parses_without_an_error() {
    assert_all_parse("C++11", CONSTRUCTS);
}

/// **A braced range** — `for (int x : {1, 2, 3})` — is C++11, and it used to be the one position where a
/// braced-init-list was not read as one.
///
/// Found by the table above, which used it as the range's source and reported `expected primary expression` at the
/// opening brace. Kept as its own case rather than folded into the table: the ordinary range (`values`) is the
/// mainstream spelling and is in the table, and this one pins the rule — a `{` where an expression would be is a
/// list, exactly as it is in an argument, on an assignment's right side and in a declaration's initializer.
#[test]
fn a_braced_range_parses() {
    let tree = cpp_parser::CppParser::parse(
        "void f() { for (int x : {1, 2, 3}) { (void)x; } }",
        cpp_parser::ParserConfig::default(),
    );
    assert_eq!(tree.get_errors(), []);
}
