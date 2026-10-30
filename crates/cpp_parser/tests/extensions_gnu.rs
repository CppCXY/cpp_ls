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

/// **The corpus's own GNU shapes** — a label as a value, a computed `goto`, and a case range with a negative end.
///
/// ```c
/// static void *const TABLE[] = {&&state_zero, &&state_one};   // `&&label`: the address of a label
/// goto *TABLE[state];                                        // computed `goto`
/// case -5 ... -1:                                            // a range whose left end is negative
/// ```
///
/// Measured on a nine-file C corpus: four syntax errors in `gnu.c`, every one of them one of these three shapes (and
/// the fourth a `typeof(struct point)`, which the other test covers).
#[test]
fn a_computed_goto_and_a_negative_case_range_parse() {
    assert_all_parse(
        "GNU control flow",
        &[
            (
                "the address of a label",
                "void f(void) { static void *const table[] = {&&here}; goto *table[0]; here: return; }",
            ),
            (
                "a case range with a negative end",
                "void f(int n) { switch (n) { case -5 ... -1: break; default: break; } }",
            ),
        ],
    );
}

/// **An array-typed compound literal** — `(const char[]){"compound"}` and `(int[]){1, 2}`.
///
/// C99's compound literal with an *array* type: the type name ends in `[]`, and the braced list that follows is the
/// value. The object form (`(point){.x = 3}`) has been read since neovim's `CSTR_AS_OBJ` was reported, so the array
/// spelling is the part that was missing — measured on a nine-file C corpus, `aggregates.c`'s one syntax error, and the
/// reason every member of that file went unoffered.
#[test]
fn an_array_typed_compound_literal_parses() {
    assert_all_parse(
        "compound literals",
        &[
            (
                "an array type",
                "void f(void) { const char *text = (const char[]){\"compound\"}; (void)text; }",
            ),
            (
                "an array of ints, subscripted",
                "void f(int index) { int n = ((int[]){1, 2})[index]; (void)n; }",
            ),
        ],
    );
}

/// **`typeof` and `__typeof__` take a *type*, not only an expression** — C23 and GNU.
///
/// ```c
/// typeof(struct point) origin = {3, 4};
/// __typeof__(const int) frozen = 1;
/// typeof(value) copy = value;          // the expression form, which already parsed
/// ```
///
/// The specifier rule read `decltype`'s payload as an expression, and a `struct` cannot begin one, so the declaration
/// failed — measured on a nine-file C corpus, six syntax errors over two files (`c23.c`, `gnu.c`), and with them every
/// member and type answer about those files.
#[test]
fn a_typeof_specifier_takes_a_type() {
    assert_all_parse(
        "typeof",
        &[
            (
                "typeof with an elaborated type",
                "void f(void) { typeof(struct point) origin = {3, 4}; (void)origin; }",
            ),
            (
                "__typeof__ with a qualified type",
                "void f(void) { __typeof__(const int) frozen = 1; (void)frozen; }",
            ),
            (
                "typeof with an expression",
                "void f(int value) { typeof(value) copy = value; (void)copy; }",
            ),
        ],
    );
}

/// **`_BitInt` as a declaration's own type, and as a cast inside its initialiser** — C23.
///
/// ```c
/// _BitInt(37) wide = (_BitInt(37))1 << 30;
/// unsigned _BitInt(7) small = 100;
/// ```
///
/// The first spelling had no declaration anchor at all: `_BitInt` is a contextual keyword (an identifier followed by a
/// `(`), so the statement was read as an expression. Adding the anchor made this line fail — the initialiser's cast
/// `(_BitInt(37))` needs the same rule in `is_a_type_in_parentheses` — which is why both halves are pinned here together.
/// Measured on a nine-file C corpus: `struct bits` answered one of its two members.
#[test]
fn a_bitint_type_in_a_declaration_and_in_a_cast_parses() {
    // **C23's language, because `_BitInt` is C23's.** `assert_all_parse` parses with `ParserConfig::default()`, which is
    // C++17 — and in C++17 `_BitInt` is an ordinary identifier, so every one of these shapes is *supposed* to fail there.
    // The first version of this test asked the default and reported a failure that was the harness's, not the grammar's.
    use cpp_parser::features::LanguageConfig;
    let language = LanguageConfig::from_flag("-std=c23").expect("a flag");
    let mut failed = Vec::new();
    for (name, source) in [
        (
            "a declaration and a cast",
            "void f(void) { _BitInt(37) wide = (_BitInt(37))1 << 30; (void)wide; }",
        ),
        (
            "with an unsigned specifier in front",
            "void f(void) { unsigned _BitInt(7) small = 100; (void)small; }",
        ),
        (
            "a bit-field in a struct",
            "struct bits { unsigned _BitInt(3) three : 3; _BitInt(13) signed_wide : 13; };",
        ),
    ] {
        let config = cpp_parser::ParserConfig::default().with_language(language);
        let tree = cpp_parser::CppParser::parse(source, config);
        if !tree.get_errors().is_empty() {
            failed.push(format!("{name}: {:?}", tree.get_errors()));
        }
    }
    assert!(failed.is_empty(), "C23 _BitInt: {failed:#?}");
}
