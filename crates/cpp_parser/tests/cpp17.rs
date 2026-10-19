//! **C++17 syntax** — structured bindings, `if constexpr`, and the fold.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    (
        "a structured binding",
        "struct S { int a; int b; }; void f() { auto [a, b] = S{1, 2}; }",
    ),
    (
        "if constexpr",
        "template <class T> void f(T t) { if constexpr (sizeof(T) > 4) { (void)t; } }",
    ),
    (
        "an inline variable",
        "struct S { static inline int n = 0; };",
    ),
    (
        "a fold expression",
        "template <class... Ts> auto f(Ts... values) { return (values + ...); }",
    ),
    (
        "a non-type template parameter of auto",
        "template <auto N> struct S { };",
    ),
    (
        "class template argument deduction",
        "template <class T> struct Box { Box(T); }; void f() { Box b{1}; }",
    ),
    ("a nested namespace", "namespace a::b::c { int n; }"),
    ("[[nodiscard]]", "[[nodiscard]] int f();"),
    (
        "[[maybe_unused]]",
        "void f() { [[maybe_unused]] int n = 1; }",
    ),
    (
        "[[fallthrough]]",
        "void f(int n) { switch (n) { case 1: [[fallthrough]]; default: break; } }",
    ),
    (
        "an if with an initialiser",
        "void f() { if (int n = 1; n > 0) { (void)n; } }",
    ),
    (
        "a constexpr lambda",
        "constexpr auto g = [](int n) { return n; };",
    ),
    (
        "a noexcept operator",
        "void f() { bool b = noexcept(1 + 1); (void)b; }",
    ),
    (
        "an attribute on a namespace",
        "namespace [[deprecated]] old { int n; }",
    ),
];

#[test]
fn every_cpp17_construct_parses_without_an_error() {
    assert_all_parse("C++17", CONSTRUCTS);
}
