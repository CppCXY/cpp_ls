//! **`assign-type-check`** — a value that does not convert to the type it is assigned to, and the shapes that must
//! stay silent.
//!
//! The silent half is the contract this file mostly documents: a braced initialiser, a construction, a default
//! argument, a deduced `auto`, and — the case this file was extended for — **text another file's macro body pasted
//! in**. The rule asks the reading, not the spelling: the cook records, per token, whether *this file* wrote the
//! spelling, so a declaration that came out of a header's macro body is `was_written_here == false` and is not judged.

use super::{codes_in, codes_of};
use crate::DiagnosticCode;

/// And the rule still reports what the file itself wrote: a string literal where an `int` is declared.
#[test]
fn a_string_assigned_to_an_int_is_still_reported() {
    let codes = codes_of("void f(void) {\n    int count = \"three\";\n}\n");
    assert!(
        codes.contains(&DiagnosticCode::AssignTypeCheck),
        "a string literal does not convert to an int: {codes:?}"
    );
}

/// **In C a string literal is `char[]`, not `const char[]`** — so `char *p = "…"` is an exact conversion there.
///
/// C11 6.4.5p6: "the multibyte character sequence is then used to initialize an array of static storage duration and
/// length just sufficient to contain the sequence" — no `const` anywhere. In C++ it is `const char[N]`, and assigning
/// it to a `char *` is a discarded qualifier (ill-formed since C++11), which is why the C++ half of this test still
/// reports. Measured on redis 8.x: eleven `assign-type-check`, eight `return-type-mismatch` and one `param-type-check`
/// finding, every one of them this pair (`docs/c-support.md` §8).
#[test]
fn a_string_literal_in_a_char_pointer_position_is_c_only() {
    let source = "char *f(void) {\n    char *p = \"redis\";\n    return p;\n}\n";
    let in_c = codes_in(&[("/p/a.c", source)], "/p/a.c");
    assert!(
        !in_c.contains(&DiagnosticCode::AssignTypeCheck),
        "in C a literal is `char[]`, so this converts: {in_c:?}"
    );
    let in_cpp = codes_in(&[("/p/a.cpp", source)], "/p/a.cpp");
    assert!(
        in_cpp.contains(&DiagnosticCode::AssignTypeCheck),
        "in C++ the same line discards a qualifier and is reported: {in_cpp:?}"
    );
}

/// **The assignment half asks the same question.** A value assigned through a name this file wrote is judged; the
/// same shape produced by another file's macro is not.
#[test]
fn an_assignment_the_file_wrote_is_still_judged() {
    let codes = codes_of("void f(void) {\n    int count = 0;\n    count = \"four\";\n}\n");
    assert!(
        codes.contains(&DiagnosticCode::AssignTypeCheck),
        "a string assigned to an int is the assignment half of this rule: {codes:?}"
    );
}

/// **A pointer converts to `bool`** — a standard scalar conversion in C and C++. `tui.c` declares
/// `bool setrgbf = tui->ti.defs[kTerm_set_rgb_foreground];`, where the element is `const char *`; reporting it
/// refused was a false positive on every capability lookup of that shape.
#[test]
fn a_pointer_to_bool_is_a_standard_conversion() {
    let source = "const char *s = \"x\";\nvoid f(void) {\n    bool b = s;\n}\n";
    let in_c = codes_in(&[("/p/a.c", source)], "/p/a.c");
    assert!(
        !in_c.contains(&DiagnosticCode::AssignTypeCheck),
        "a pointer to `_Bool` is a standard conversion: {in_c:?}"
    );
    let in_cpp = codes_in(&[("/p/a.cpp", source)], "/p/a.cpp");
    assert!(
        !in_cpp.contains(&DiagnosticCode::AssignTypeCheck),
        "and the same in C++: {in_cpp:?}"
    );
}

/// **An assignment whose right operand is a conditional is not an assignment of the condition.** `m = ptr ? a : b`
/// is `m = (ptr ? a : b)`; the old parser read `(m = ptr) ? a : b`, so `assign-type-check` asked whether `ptr`'s
/// pointer type converted to `m`'s `int` — neovim's `ex_getln.c:3055` (`m = cmdline_win ? … : Columns * Rows`) was
/// reported for a perfectly valid C expression.
#[test]
fn a_pointer_conditioned_ternary_is_not_an_assignment_of_the_condition() {
    let source = "\
struct win { int width; };\n\
extern struct win *cmdline_win;\n\
int f(void) {\n\
    int m;\n\
    m = cmdline_win ? 1 * 2 : 3 * 4;\n\
    return m;\n\
}\n";
    let codes = codes_of(source);
    assert!(
        !codes.contains(&DiagnosticCode::AssignTypeCheck),
        "the conditional's branches are `int`, and `m` is `int`: {codes:?}"
    );
}

/// **The object an assignment writes to has to be writable at all** — a `const` pointee and an array are both
/// refused by every compiler, and neither is a question about the value.
///
/// Measured on a C project of twenty-five files with one deliberate defect each, both silent:
/// `*value = 1` through a `const int *`, and `buffer = "text"` on a local array.
#[test]
fn a_target_that_cannot_be_written_is_reported() {
    let through_a_const_pointer = codes_of("void f(const int *value) {\n    *value = 1;\n}\n");
    assert!(
        through_a_const_pointer.contains(&DiagnosticCode::AssignTypeCheck),
        "the pointee is const: {through_a_const_pointer:?}"
    );

    let on_an_array = codes_of("void f(void) {\n    char buffer[8];\n    buffer = \"text\";\n}\n");
    assert!(
        on_an_array.contains(&DiagnosticCode::AssignTypeCheck),
        "an array is not assignable: {on_an_array:?}"
    );
}

/// **…and every writable target stays silent**: the ordinary `*p = 1`, an element, a member, and a reference.
#[test]
fn a_writable_target_is_silent() {
    let sources = [
        (
            "through a pointer",
            "void f(int *pointer) {\n    *pointer = 1;\n}\n",
        ),
        (
            "through an index",
            "void f(void) {\n    char buffer[8];\n    buffer[0] = 'x';\n}\n",
        ),
        (
            "through a member",
            "struct point { int x; };\nvoid f(point *p) {\n    p->x = 1;\n}\n",
        ),
        (
            "through a reference",
            "void f(int &value) {\n    value = 1;\n}\n",
        ),
        (
            "a pointer to const data is still a pointer that can move",
            "void f(const char *text) {\n    text = 0;\n}\n",
        ),
    ];
    for (what, source) in sources {
        let codes = codes_of(source);
        assert!(
            !codes.contains(&DiagnosticCode::AssignTypeCheck),
            "{what}: {codes:?}"
        );
    }
}
