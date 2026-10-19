//! **C++23 syntax**, as a project targeting the current standard would write it.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    (
        "if consteval",
        "int f(int n) { if consteval { return 1; } else { return n; } }",
    ),
    ("consteval", "consteval int f() { return 1; }"),
    ("constinit", "constinit int g = 0;"),
    (
        "[[assume]]",
        "int f(int n) { [[assume(n > 0)]]; return n; }",
    ),
    ("deducing this", "struct S { void f(this S&& self); };"),
    (
        "a multidimensional subscript",
        "int f(int (&a)[2][2]) { return a[1, 1]; }",
    ),
    (
        "a static operator()",
        "struct F { static void operator()(int); };",
    ),
    (
        "a static operator[]",
        "struct F { static int& operator[](int); };",
    ),
    (
        "a lambda without a parameter list",
        "auto f = [] { return 1; };",
    ),
    (
        "auto(x), the decay-copy",
        "void f(int x) { auto y = auto(x); }",
    ),
    ("#elifdef", "#ifdef A\nint a;\n#elifdef B\nint b;\n#endif\n"),
    (
        "#elifndef",
        "#ifndef A\nint a;\n#elifndef B\nint b;\n#endif\n",
    ),
    ("#warning", "#warning \"this is not an error\"\nint a;\n"),
    (
        "an explicit object parameter on a template",
        "struct S { template <class Self> void f(this Self&& self); };",
    ),
];

#[test]
fn every_cpp23_construct_parses_without_an_error() {
    assert_all_parse("C++23", CONSTRUCTS);
}
