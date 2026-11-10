//! **`undefined-field`** — a member access on a type whose member list this file read, which does not hold that name.
//!
//! The rule's direction of error is "only a definite no": the member list must have been read, it must be complete,
//! and the class must be one this file writes. A type from a header, a template parameter, a list with an unread base
//! and an arrow whose pointee cannot be typed are all "not read" rather than "not there", and each is silence below —
//! while the two positives are the spellings that can be judged, a dot and an arrow.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain case**: a class this file declares, and a member it does not.
#[test]
fn a_member_the_class_does_not_declare_is_reported() {
    let codes = codes_of("struct C { int n; };\nvoid g() { C x; x.m; }\n");
    assert_reported(
        &codes,
        DiagnosticCode::UndefinedField,
        "`C` declares `n` and no `m`",
    );
}

/// **An arrow is followed to the class it reaches.** `session->Missing()` asks about `Session`, not about the
/// pointer's own members — which is why the answer is `Session`'s member list.
#[test]
fn an_arrow_is_judged_against_the_class_it_reaches() {
    let codes = codes_of(
        "struct Session { void Run(); };\nvoid g(Session *session) { session->Missing(); }\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::UndefinedField,
        "`Session` has no `Missing`",
    );
}

/// **A member that is there is silent**, on both spellings — the negative that keeps the positive honest.
#[test]
fn a_member_the_class_declares_is_silent() {
    let dot = codes_of("struct C { int n; };\nvoid g() { C x; x.n; }\n");
    assert_silent_about(&dot, DiagnosticCode::UndefinedField, "`C` declares `n`");

    let arrow =
        codes_of("struct Session { void Run(); };\nvoid g(Session *session) { session->Run(); }\n");
    assert_silent_about(
        &arrow,
        DiagnosticCode::UndefinedField,
        "`Run` is the pointee's own member",
    );
}

/// **A template parameter has no member list to check against.** `C x; x.m;` inside a template is decided at
/// instantiation, and asking the index for `C`'s members invents the answer "a class with nothing in it".
#[test]
fn a_dependent_type_is_not_judged() {
    let codes = codes_of("template <class C>\nvoid f() { C x; x.m; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::UndefinedField,
        "`C` names a template parameter, so its members exist only at instantiation",
    );
}

/// **A member list this file cannot complete is not evidence.** `std::string` is declared in a header this file does
/// not write, and `D`'s base `Missing` was never read — so a name absent from either list is "not read" rather than
/// "not there".
#[test]
fn a_member_list_this_file_cannot_complete_is_silent() {
    let elsewhere = codes_of("void g(std::string full) { full.size(); }\n");
    assert_silent_about(
        &elsewhere,
        DiagnosticCode::UndefinedField,
        "`std::string`'s members are in a header this file does not write",
    );

    let partial =
        codes_of("struct D : Missing { void f(); };\nvoid g(D d) { d.not_read_anywhere(); }\n");
    assert_silent_about(
        &partial,
        DiagnosticCode::UndefinedField,
        "`Missing` was never read, so `D`'s member list is partial",
    );
}

/// **An arrow whose pointee cannot be typed says nothing** — the receiver's own list is not the answer, and
/// `operator->`'s return type is what would have to be read.
#[test]
fn an_arrow_with_no_typed_pointee_is_silent() {
    let codes = codes_of(
        "struct Session { void Run(); };\n\
         struct Pointer { Missing *operator->(); };\n\
         void g(Pointer pointer) { pointer->Run(); }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::UndefinedField,
        "the pointee cannot be typed, so the member is not judged",
    );
}
