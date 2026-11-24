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
