//! **`dangling-field`** — a member that borrows something which does not live as long as the object.
//!
//! Two shapes report: an in-class default initialiser and a constructor's member initialiser, each when what is stored
//! is a **by-value parameter** (destroyed at the constructor's closing brace) or a **temporary** (destroyed at the end
//! of the statement). A reference parameter — the caller's obligation — a member of `*this`, a string literal, an
//! owning member and a pointer whose target is unknown all stay silent, for the reasons the tests state.
//!
//! The types are local structs named `string_view` and `string`: the rule classifies a view by the **last name** of the
//! type's spelling and a temporary by the callee's name, so the fixture needs no real header.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A view member of a by-value parameter**: `s` is a copy the constructor destroys before the object exists.
#[test]
fn a_view_member_of_a_by_value_parameter_is_reported() {
    let codes = codes_of(
        "struct string {};\nstruct string_view {};\nstruct S {\n    string_view v;\n    S(string s) : v(s) {}\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::DanglingField,
        "`s` is a parameter that dies when the constructor returns",
    );
}

/// **A view member of a temporary**: the temporary dies at the end of the statement that built the object.
#[test]
fn a_view_member_of_a_temporary_is_reported() {
    let codes = codes_of(
        "struct string {};\nstruct string_view {};\nstruct S {\n    string_view v = string(\"x\");\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::DanglingField,
        "`string(\"x\")` is a temporary",
    );
}

/// **A reference member of a temporary** is the same defect with a different kind of borrow.
#[test]
fn a_reference_member_of_a_temporary_is_reported() {
    let codes = codes_of(
        "struct string {};\nstruct S {\n    const string& v;\n    S() : v(string(\"x\")) {}\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::DanglingField,
        "the member refers to a temporary",
    );
}

/// **A pointer member taken from the parameter's own buffer**: `s.c_str()` hands out storage `s` owns.
#[test]
fn a_pointer_member_of_a_parameters_buffer_is_reported() {
    let codes = codes_of(
        "struct string {};\nstruct S {\n    const char* p;\n    S(string s) : p(s.c_str()) {}\n};\n",
    );
    assert_reported(
        &codes,
        DiagnosticCode::DanglingField,
        "the pointer refers into `s`",
    );
}

/// **The shapes that outlive the object stay silent**: an owning member, a reference parameter, and a member of
/// `*this`.
#[test]
fn an_owning_member_a_reference_parameter_and_this_are_silent() {
    let owning =
        codes_of("struct string {};\nstruct S {\n    string v;\n    S(string s) : v(s) {}\n};\n");
    assert_silent_about(
        &owning,
        DiagnosticCode::DanglingField,
        "`v` owns its copy, so there is nothing to dangle",
    );

    let reference_parameter = codes_of(
        "struct string {};\nstruct string_view {};\nstruct S {\n    string_view v;\n    S(const string& s) : v(s) {}\n};\n",
    );
    assert_silent_about(
        &reference_parameter,
        DiagnosticCode::DanglingField,
        "the caller owns what `s` refers to",
    );

    let member_of_this = codes_of(
        "struct string {};\nstruct string_view {};\nstruct S {\n    string s;\n    string_view v;\n    S() : v(s) {}\n};\n",
    );
    assert_silent_about(
        &member_of_this,
        DiagnosticCode::DanglingField,
        "`s` is a member and outlives the view of it",
    );
}

/// **A string literal has static storage, and a pointer parameter may point anywhere** — neither is a borrow this could
/// prove dangling.
#[test]
fn a_string_literal_and_an_unknown_pointer_are_silent() {
    let literal =
        codes_of("struct string_view {};\nstruct S {\n    string_view v = \"text\";\n};\n");
    assert_silent_about(
        &literal,
        DiagnosticCode::DanglingField,
        "the array a literal names outlives the object",
    );

    let unknown = codes_of("struct S {\n    int* p;\n    S(int* q) : p(q) {}\n};\n");
    assert_silent_about(
        &unknown,
        DiagnosticCode::DanglingField,
        "`q` may point at anything, and the rule does not guess",
    );
}

/// **A view member of a view parameter** — the reported shape, from `EmmyLuaCodeStyle`'s `spell::Word`:
///
/// ```cpp
/// struct Word {
///     TextRange Range;
///     std::string_view Item;
///     Word(TextRange range, std::string_view item) : Range(range), Item(item) {}
/// };
/// ```
///
/// `item` is a by-value parameter, but it is a **view**: the bytes it names belong to the caller, and copying it into
/// the member copies a pointer and a length. It is the same argument [`Borrowed::owner_is_a_view`] already makes for a
/// view *local* in `dangling-return` — and it was reported as "a parameter that is destroyed", which is true of the
/// parameter object and false of what the parameter refers to.
#[test]
fn a_view_member_of_a_view_parameter_is_silent() {
    let view = codes_of(
        "struct string_view {};\nstruct Word {\n    string_view Item;\n    Word(string_view item) : Item(item) {}\n};\n",
    );
    assert_silent_about(
        &view,
        DiagnosticCode::DanglingField,
        "a view parameter names the caller's bytes; the member copies the view, not the parameter",
    );

    // **A pointer parameter is the same shape**: `const char* p` owns nothing either, and a view built from it names
    // whatever the caller passed.
    let pointer = codes_of(
        "struct string_view {};\nstruct S {\n    string_view v;\n    S(const char *p) : v(p) {}\n};\n",
    );
    assert_silent_about(
        &pointer,
        DiagnosticCode::DanglingField,
        "the characters a pointer parameter names are the caller's",
    );
}
