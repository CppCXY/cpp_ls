//! **`assign-type-check`** — a value that does not convert to the type it is assigned to, and the shapes that must
//! stay silent.
//!
//! The silent half is the contract this file mostly documents: a braced initialiser, a construction, a default
//! argument, a deduced `auto`, and — the case this file was extended for — **text another file's macro body pasted
//! in**. The rule asks the reading, not the spelling: the cook records, per token, whether *this file* wrote the
//! spelling, so a declaration that came out of a header's macro body is `was_written_here == false` and is not judged.

use super::codes_of;
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
