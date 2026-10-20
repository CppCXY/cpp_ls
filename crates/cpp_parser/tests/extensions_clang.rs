//! **Clang extensions** — blocks, the nullability qualifiers, and `__has_feature`.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("_Nullable", "void f(int* _Nullable p);"),
    ("_Nonnull", "void f(int* _Nonnull p);"),
    ("_Null_unspecified", "void f(int* _Null_unspecified p);"),
    (
        "__has_feature",
        "#if __has_feature(cxx_concepts)\nint n;\n#endif\n",
    ),
    (
        "__has_include",
        "#if __has_include(<vector>)\nint n;\n#endif\n",
    ),
    (
        "__has_builtin",
        "#if __has_builtin(__builtin_expect)\nint n;\n#endif\n",
    ),
    ("a clang attribute", "[[clang::fallthrough]] void f();"),
    (
        "__attribute__ with a clang spelling",
        "__attribute__((no_sanitize(\"address\"))) void f();",
    ),
    (
        "overloadable",
        "__attribute__((overloadable)) int f(int n);",
    ),
    ("__auto_type", "void f() { __auto_type n = 1; (void)n; }"),
    (
        "a vector type",
        "typedef int int4 __attribute__((ext_vector_type(4)));",
    ),
    (
        "a #pragma clang diagnostic",
        "#pragma clang diagnostic push\n#pragma clang diagnostic ignored \"-Wall\"\nint n;\n#pragma clang diagnostic pop\n",
    ),
];

#[test]
fn every_clang_extension_parses_without_an_error() {
    assert_all_parse("Clang", CONSTRUCTS);
}

/// **A block** — `void (^block)(int) = ^(int n) { … };` — Clang's closure syntax, in its declaration and its
/// literal.
///
/// It is the extension the whole of Apple's C API is written with, and both halves used to fail:
///
/// ```text
///   void (^handler)(void)         a block *declaration* — the same shape a function pointer has, with `^` where
///                                 the `*` would be; it reported ``expected `;` after expression`` at the `)`
///   ^(int n) { (void)n; }         a block *literal* — a caret where an expression begins
/// ```
///
/// The declarator is read as the pointer-shaped node the `^` really is, and the literal as a `LambdaExpr`: a block
/// is an anonymous function with no capture list, which is the closest thing this grammar has. Nothing is invented
/// — the `^`, the parameter list and the body are the file's own tokens.
#[test]
fn a_block_literal_parses() {
    assert_all_parse(
        "Clang blocks",
        &[(
            "a block",
            "void f() { void (^block)(int) = ^(int n) { (void)n; }; (void)block; }",
        )],
    );
}
