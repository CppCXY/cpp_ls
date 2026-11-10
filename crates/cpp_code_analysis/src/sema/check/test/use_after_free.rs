//! **`use-after-free`** — memory that was released and is then read through the pointer.
//!
//! The analysis is a **must**-analysis: a pointer is freed after a statement only if every path to it freed it, so a
//! free under an `if`, a free inside a loop and a pointer given a fresh value are all silence — reporting them would
//! be reporting code that compiles and runs. A use is a read of the pointee — `p->x`, `*p`, `p[i]` — and a `.` is not
//! one, because it reads the pointer object rather than what the pointer refers to.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain shape**: `delete` and then a read through the pointer.
#[test]
fn a_pointer_read_after_delete_is_reported() {
    let codes = codes_of(
        "struct T { int x; };\nvoid f() {\n    T* p = new T();\n    delete p;\n    p->x;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::UseAfterFree,
        "`p`'s object was deleted",
    );
}

/// **The inter-procedural case**: the free is written in another function and recorded on that function's fact,
/// because a call site cannot see the callee's body.
#[test]
fn a_pointer_read_after_a_helper_frees_it_is_reported() {
    let codes = codes_of(
        "struct T { int x; };\n\
         void release(T* p) { delete p; }\n\
         void f() {\n    T* p = new T();\n    release(p);\n    p->x;\n}\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::UseAfterFree,
        "`release` frees its parameter, and the call site knows it",
    );
}

/// **`free` and `reset` are frees too**: the C function releases the allocation, and a smart pointer's `reset`
/// releases its pointee — `u` itself stays valid, but `*u` does not.
#[test]
fn free_and_reset_are_frees() {
    let freed = codes_of("void f() {\n    int* p = (int*)malloc(4);\n    free(p);\n    *p;\n}\n");
    assert_reported(
        &freed,
        DiagnosticCode::UseAfterFree,
        "`free` releases the pointee",
    );

    let reset = codes_of(
        "void f() {\n    std::unique_ptr<int> u(new int(1));\n    u.reset();\n    *u;\n}\n",
    );
    assert_reported(
        &reset,
        DiagnosticCode::UseAfterFree,
        "`reset` releases the pointee",
    );
}

/// **The must-analysis stays quiet**: a read before the free, a free that may not run, a pointer given a fresh value
/// and a callee that frees nothing are all shapes a may-analysis would report.
#[test]
fn the_safe_free_shapes_are_silent() {
    let before = codes_of(
        "struct T { int x; };\nvoid f() {\n    T* p = new T();\n    p->x;\n    delete p;\n}\n",
    );
    assert_silent_about(
        &before,
        DiagnosticCode::UseAfterFree,
        "the read is before the free",
    );

    let conditional = codes_of(
        "struct T { int x; };\n\
         void f(int c) {\n    T* p = new T();\n    if (c) { delete p; }\n    p->x;\n}\n",
    );
    assert_silent_about(
        &conditional,
        DiagnosticCode::UseAfterFree,
        "the free may not run, so the later read is only sometimes wrong",
    );

    let reassigned = codes_of(
        "struct T { int x; };\n\
         void f() {\n    T* p = new T();\n    delete p;\n    p = new T();\n    p->x;\n}\n",
    );
    assert_silent_about(
        &reassigned,
        DiagnosticCode::UseAfterFree,
        "`p` was given a fresh value, so the freed object is not the one read",
    );

    let untouched = codes_of(
        "struct T { int x; };\n\
         void touch(T* p) { }\n\
         void f() {\n    T* p = new T();\n    touch(p);\n    p->x;\n}\n",
    );
    assert_silent_about(
        &untouched,
        DiagnosticCode::UseAfterFree,
        "the callee's fact does not say it frees",
    );
}

/// **A `.` is not a read through the pointer**: `h.reset()` frees `h`'s pointee, and `h.value` reads `h` itself,
/// which a smart pointer after `reset` leaves null rather than dangling.
#[test]
fn a_dot_is_not_a_read_through_the_pointer() {
    let codes = codes_of(
        "struct Holder {\n    void reset();\n    int value;\n};\n\
         void f() {\n    Holder h;\n    h.reset();\n    h.value;\n}\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::UseAfterFree,
        "`h.value` reads the pointer object, not its pointee",
    );
}
