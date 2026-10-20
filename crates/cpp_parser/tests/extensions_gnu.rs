//! **GNU extensions** — what `-std=gnu++` accepts and a project's headers use.
//!
//! Three of the fourteen do not parse today; they are in the ignored test at the bottom, so the table above stays a
//! statement of what does, and the gaps are a statement of what does not.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("__attribute__", "__attribute__((always_inline)) void f();"),
    (
        "__attribute__ after the declarator",
        "int f() __attribute__((pure));",
    ),
    ("__typeof__", "void f() { __typeof__(1) n = 1; }"),
    ("typeof", "void f() { typeof(1) n = 1; }"),
    ("__extension__", "__extension__ typedef int T;"),
    ("an asm block", "void f() { __asm__ volatile (\"nop\"); }"),
    ("__int128", "__int128 big = 0;"),
    ("__restrict__", "void f(int* __restrict__ a);"),
    (
        "a case range",
        "void f(int n) { switch (n) { case 1 ... 5: break; default: break; } }",
    ),
    (
        "__builtin_expect",
        "void f(int n) { if (__builtin_expect(n, 1)) { return; } }",
    ),
    ("#include_next", "#include_next <stdio.h>\n"),
    (
        "__asm__ as a declaration's specifier",
        "__asm__(\".globl f\");",
    ),
];

#[test]
fn every_gnu_extension_parses_without_an_error() {
    assert_all_parse("GNU", CONSTRUCTS);
}

/// **The GNU constructs that used to be gaps** — now part of the table, kept as their own test because each was a
/// deliberate decision rather than a spelling.
///
/// ```text
///   int n = ({ int a = 1; a; });   statement expression: a compound statement where an expression would be
///   typeof(1) n = 1;               `typeof` is lexed as `decltype`
///   int n = a ?: b;                the elvis operator: the middle operand is the condition, written once
/// ```
///
/// The first and the third are what glibc's and the Linux headers' macros are built on, so a parse of a real
/// project on a GNU toolchain meets them. The statement expression is read as the **compound statement it is**, and
/// the elvis keeps its two operands with no middle one — nothing is synthesised into the tree, because the value
/// `a ?: b` yields is `a` and a copy of its tokens would be text the file wrote once.
#[test]
fn the_gnu_gaps_are_named() {
    assert_all_parse(
        "GNU gaps",
        &[
            (
                "a statement expression",
                "void f() { int n = ({ int a = 1; a; }); (void)n; }",
            ),
            (
                "an elvis operator",
                "void f(int a, int b) { int n = a ?: b; (void)n; }",
            ),
        ],
    );
}
