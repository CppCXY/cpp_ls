//! **C++14 syntax** — the release that made `auto` ordinary.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("a generic lambda", "void f() { auto g = [](auto n) { return n; }; }"),
    ("a deduced return type", "auto f() { return 1; }"),
    ("decltype(auto)", "decltype(auto) f() { return 1; }"),
    ("a variable template", "template <class T> constexpr T pi = T(3);"),
    ("a binary literal", "int n = 0b1010;"),
    ("digit separators", "int n = 1'000'000;"),
    ("a relaxed constexpr function", "constexpr int f(int n) { int total = 0; for (int i = 0; i < n; ++i) { total += i; } return total; }"),
    ("an init-capture", "void f() { auto g = [n = 1] { return n; }; }"),
    ("a lambda with auto parameters and a trailing return", "void f() { auto g = [](auto n) -> int { return n; }; }"),
];

#[test]
fn every_cpp14_construct_parses_without_an_error() {
    assert_all_parse("C++14", CONSTRUCTS);
}