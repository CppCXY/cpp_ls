//! **C++20 syntax** — concepts, coroutines, modules, and the spaceship.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    (
        "a concept",
        "template <class T> concept Small = sizeof(T) <= 4;",
    ),
    (
        "a requires clause",
        "template <class T> requires Small<T> void f(T t);",
    ),
    ("an abbreviated template", "void f(Small auto n);"),
    (
        "a requires expression",
        "template <class T> concept Has = requires(T t) { t.size(); };",
    ),
    (
        "a coroutine's co_await",
        "struct A { bool await_ready(); }; void f(A a) { co_await a; }",
    ),
    ("a coroutine's co_return", "void f() { co_return; }"),
    ("a coroutine's co_yield", "void f() { co_yield 1; }"),
    ("a module", "export module m;\nint n;"),
    ("an import", "import m;\nint n;"),
    (
        "a designated initialiser",
        "struct S { int a; int b; }; void f() { S s{ .a = 1, .b = 2 }; }",
    ),
    (
        "the spaceship operator",
        "struct S { bool operator<=>(const S&) const; };",
    ),
    ("char8_t", "char8_t c = u8'a';"),
    (
        "[[likely]]",
        "void f(int n) { if (n > 0) [[likely]] { return; } }",
    ),
    (
        "[[no_unique_address]]",
        "struct S { [[no_unique_address]] int n; };",
    ),
    (
        "using enum",
        "enum class E { A }; void f() { using enum E; }",
    ),
    (
        "explicit(bool)",
        "template <class T> struct S { explicit(sizeof(T) > 4) S(T); };",
    ),
    (
        "a constexpr virtual function",
        "struct S { virtual constexpr int f(); };",
    ),
    // **A labelled statement may begin with an attribute**, and `[[likely]] case 1:` is how a reader marks the
    // branch expected to win — `[stmt.label]` spells the attribute-specifier-seq on the label itself.
    (
        "an attribute on a case label",
        "void f(int n) { switch (n) { [[likely]] case 1: break; default: break; } }",
    ),
    // **A pack capture puts the `...` before the name** — the `init-capture` form, which is what a forwarding
    // lambda over `std::apply`'s arguments is written with.
    // **`operator co_await`** — a keyword as an operator's name, which is how a type says it is awaitable.
    (
        "an awaitable's own operator co_await",
        "struct Awaiter { class Handle {}; Handle handle; Awaiter operator co_await() { return *this; } };",
    ),
    (
        "a pack capture",
        "template <typename... Ts> void f(Ts... xs) { auto g = [... copies = xs] { return 0; }; }",
    ),
    // **A range-for whose range is a `views::` expression**: a call, a pipe, or a braced temporary followed by one.
    // The colon was stepped over by the qualified-name scan, so the declaration's own name joined the type and the
    // header fell back to the C-style reading.
    (
        "a range-for over a views call",
        "void f() { for (int v : std::views::iota(1, 6)) { (void)v; } }",
    ),
    (
        "a range-for over a pipe",
        "void f() { for (int v : values | std::views::reverse) { (void)v; } }",
    ),
    (
        "a range-for over a braced temporary and a pipe",
        "void f() { for (auto part : std::string_view{text} | std::views::split(',')) { (void)part; } }",
    ),
];

#[test]
fn every_cpp20_construct_parses_without_an_error() {
    assert_all_parse("C++20", CONSTRUCTS);
}
