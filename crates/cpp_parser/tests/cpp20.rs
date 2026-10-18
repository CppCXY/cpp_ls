//! **C++20 syntax** — concepts, coroutines, modules, and the spaceship.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("a concept", "template <class T> concept Small = sizeof(T) <= 4;"),
    ("a requires clause", "template <class T> requires Small<T> void f(T t);"),
    ("an abbreviated template", "void f(Small auto n);"),
    ("a requires expression", "template <class T> concept Has = requires(T t) { t.size(); };"),
    ("a coroutine's co_await", "struct A { bool await_ready(); }; void f(A a) { co_await a; }"),
    ("a coroutine's co_return", "void f() { co_return; }"),
    ("a coroutine's co_yield", "void f() { co_yield 1; }"),
    ("a module", "export module m;\nint n;"),
    ("an import", "import m;\nint n;"),
    ("a designated initialiser", "struct S { int a; int b; }; void f() { S s{ .a = 1, .b = 2 }; }"),
    ("the spaceship operator", "struct S { bool operator<=>(const S&) const; };"),
    ("char8_t", "char8_t c = u8'a';"),
    ("[[likely]]", "void f(int n) { if (n > 0) [[likely]] { return; } }"),
    ("[[no_unique_address]]", "struct S { [[no_unique_address]] int n; };"),
    ("using enum", "enum class E { A }; void f() { using enum E; }"),
    ("explicit(bool)", "template <class T> struct S { explicit(sizeof(T) > 4) S(T); };"),
    ("a constexpr virtual function", "struct S { virtual constexpr int f(); };"),
];

#[test]
fn every_cpp20_construct_parses_without_an_error() {
    assert_all_parse("C++20", CONSTRUCTS);
}