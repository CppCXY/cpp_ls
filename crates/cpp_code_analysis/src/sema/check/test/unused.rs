//! **`unused`** — a local variable in a source file whose name is written exactly once: the declaration.
//!
//! The rule's direction of error is "quiet on anything that can be deliberate": a name beginning with `_`, a
//! `[[maybe_unused]]` variable, a type that may have a destructor, a parameter, and every declaration in a header are
//! silence. The count is over the **whole file**, so a use anywhere else — another function, a macro body — keeps a
//! name alive, and that is the direction to be wrong in.

use super::{assert_reported, assert_silent_about, codes_in, codes_of};
use crate::DiagnosticCode;

/// **The plain case, and its negative**: a builtin local nothing reads is reported; a local that is read is not.
#[test]
fn a_local_nothing_reads_is_reported() {
    let unused = codes_of("int main() { int never_used = 1; return 0; }\n");
    assert_reported(
        &unused,
        DiagnosticCode::Unused,
        "`never_used` is written once in the file",
    );

    let used = codes_of("int main() { int used = 1; return used; }\n");
    assert_silent_about(
        &used,
        DiagnosticCode::Unused,
        "`used` is read on the next line",
    );
}

/// **Two ways a reader says "I know"**: a leading underscore and the attribute.
#[test]
fn a_marked_variable_is_silent() {
    let underscored = codes_of("int main() { int _unused = 1; return 0; }\n");
    assert_silent_about(
        &underscored,
        DiagnosticCode::Unused,
        "a leading underscore is a deliberate choice",
    );

    let attributed = codes_of("void f() {\n    [[maybe_unused]] int local = 3;\n}\n");
    assert_silent_about(
        &attributed,
        DiagnosticCode::Unused,
        "the attribute is the reader answering the rule's question",
    );
}

/// **A type that is not a builtin may have a destructor**: `guard`'s whole use would be the side effect of its
/// lifetime, and the rule cannot ask the index whether the type has one.
#[test]
fn a_lifetime_object_is_silent() {
    let codes = codes_of("struct Guard { ~Guard(); };\nvoid f() {\n    Guard guard;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::Unused,
        "only a builtin's type says by itself that it has no destructor",
    );
}

/// **The count is over the whole file, not the enclosing block** — two functions writing one name each keep both
/// alive, which is the conservative direction: a spelling this rule cannot attribute may be the use that matters.
#[test]
fn a_name_written_elsewhere_in_the_file_is_silent() {
    let codes = codes_of("void f() {\n    int count = 1;\n}\nvoid g() {\n    int count = 2;\n}\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::Unused,
        "`count` is written twice in the file, so neither declaration is certainly unread",
    );
}

/// **A parameter and a file-scope variable are not locals**: a parameter is the most common deliberate unused thing
/// in C++, and a global is not a local declaration at all.
#[test]
fn a_parameter_and_a_global_are_not_locals() {
    let parameter = codes_of("void f(int unused_parameter) { }\n");
    assert_silent_about(
        &parameter,
        DiagnosticCode::Unused,
        "a parameter is not reported",
    );

    let global = codes_of("int global_value = 0;\nvoid f() { }\n");
    assert_silent_about(
        &global,
        DiagnosticCode::Unused,
        "a file-scope variable is not a local",
    );
}

/// **A header is not the rule's question**: a file included by others is used by them, and a local inside an inline
/// function is not a name any importer writes.
#[test]
fn a_header_is_not_a_source_file() {
    let codes = codes_in(
        &[("/p/a.h", "void f() {\n    int unused_in_a_header = 3;\n}\n")],
        "/p/a.h",
    );
    assert_silent_about(&codes, DiagnosticCode::Unused, "the file is a header");
}
