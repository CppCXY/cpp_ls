//! **`dangling-return`** — a reference, a pointer or a view of something that dies when the function returns.
//!
//! The rule reports only what it can **prove**, and every silent shape below is a decision from its own module note:
//! a parameter lives as long as its argument (the caller's obligation), a `static` local and a string literal outlive
//! every call, `*this`'s member belongs to the object, and a pointer whose target would need points-to analysis is not
//! evidence. The two pointer shapes that are reported are the two whose referent is certainly the local — `&x` and a
//! pointer the local handed out (`s.c_str()`).

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The plain borrowed results**: a reference to a local, a view of a local, and a reference to a temporary.
#[test]
fn a_reference_and_a_view_of_a_local_are_reported() {
    let reference = codes_of("int& f() { int x = 0; return x; }\n");
    assert_reported(
        &reference,
        DiagnosticCode::DanglingReturn,
        "`x` is the function's own local",
    );

    let view = codes_of("std::string_view f() { std::string s; return s; }\n");
    assert_reported(
        &view,
        DiagnosticCode::DanglingReturn,
        "the view outlives the string it refers to",
    );

    let temporary = codes_of("const std::string& f() { return std::string(\"x\"); }\n");
    assert_reported(
        &temporary,
        DiagnosticCode::DanglingReturn,
        "the temporary dies at the end of the return's statement",
    );
}

/// **A pointer is reported only when its target is certain** — the address of a local, or a pointer the local handed
/// out — and neither uncertain shape is reported.
#[test]
fn a_pointer_into_a_local_is_reported_and_a_pointers_target_is_not() {
    let address = codes_of("int* f() { int x = 0; return &x; }\n");
    assert_reported(
        &address,
        DiagnosticCode::DanglingReturn,
        "`&x` points at the function's own local",
    );

    let through = codes_of("const char* f() { std::string s; return s.c_str(); }\n");
    assert_reported(
        &through,
        DiagnosticCode::DanglingReturn,
        "`c_str()` points into `s`",
    );

    let a_global = codes_of("int* p = nullptr;\nint* f() { return p; }\n");
    assert_silent_about(
        &a_global,
        DiagnosticCode::DanglingReturn,
        "`p` may point at a global or at the heap, which outlives the call",
    );

    let a_null = codes_of("int* f() { return nullptr; }\n");
    assert_silent_about(
        &a_null,
        DiagnosticCode::DanglingReturn,
        "`nullptr` points at nothing",
    );
}

/// **What outlives the call is not this rule's business**: a parameter is the caller's lifetime to keep, and a
/// static local is destroyed at the end of the program rather than at the end of the call.
#[test]
fn a_parameter_and_a_static_local_are_not_reported() {
    let by_reference = codes_of("int& f(int& x) { return x; }\n");
    assert_silent_about(
        &by_reference,
        DiagnosticCode::DanglingReturn,
        "a reference parameter lives as long as its argument",
    );

    let view_of_a_parameter = codes_of("std::string_view f(const std::string& s) { return s; }\n");
    assert_silent_about(
        &view_of_a_parameter,
        DiagnosticCode::DanglingReturn,
        "the caller owns the string `s` refers to",
    );

    let by_value = codes_of("std::string_view f(std::string s) { return s; }\n");
    assert_silent_about(
        &by_value,
        DiagnosticCode::DanglingReturn,
        "the rule treats a parameter as the caller's, by value or not",
    );

    let static_local = codes_of("int& f() { static int x = 0; return x; }\n");
    assert_silent_about(
        &static_local,
        DiagnosticCode::DanglingReturn,
        "a static local outlives every call",
    );
}

/// **A value return, a literal and a member are not borrowed from this function's stack.**
#[test]
fn a_value_a_literal_and_a_member_are_not_reported() {
    let a_value = codes_of("std::string f() { std::string s; return s; }\n");
    assert_silent_about(
        &a_value,
        DiagnosticCode::DanglingReturn,
        "the result is an owner, so nothing borrows the local",
    );

    let a_literal = codes_of("std::string_view f() { return \"literal\"; }\n");
    assert_silent_about(
        &a_literal,
        DiagnosticCode::DanglingReturn,
        "a string literal is an array with static storage duration",
    );

    let a_view_member =
        codes_of("struct S { std::string name; std::string_view get() { return name; } };\n");
    assert_silent_about(
        &a_view_member,
        DiagnosticCode::DanglingReturn,
        "`*this`'s member outlives the call by the caller's obligation",
    );

    let a_reference_member = codes_of("struct S { int x; int& get() { return x; } };\n");
    assert_silent_about(
        &a_reference_member,
        DiagnosticCode::DanglingReturn,
        "the member is not the function's local",
    );
}

/// **A view of a view is not a dangling view** — a `string_view` local owns no bytes, so destroying it destroys
/// nothing — and a call whose body nobody can see has no origin this analysis can name.
#[test]
fn a_view_of_a_view_and_an_unseen_call_are_not_reported() {
    let a_copy = codes_of("std::string_view f() { std::string_view v; return v; }\n");
    assert_silent_about(
        &a_copy,
        DiagnosticCode::DanglingReturn,
        "the local view owns no storage to dangle",
    );

    let a_subrange = codes_of(
        "std::string_view g();\n\
         std::string_view f() { std::string_view s = g(); return s.substr(0, 1); }\n",
    );
    assert_silent_about(
        &a_subrange,
        DiagnosticCode::DanglingReturn,
        "a subrange of a view borrows what the view was made from, not the local",
    );

    let unseen = codes_of("int& f();\nint& g() { return f(); }\n");
    assert_silent_about(
        &unseen,
        DiagnosticCode::DanglingReturn,
        "a call whose body nobody can see is not evidence",
    );
}

/// **A `return` inside a lambda belongs to the lambda**, whose result type is the closure's own — the rule says
/// nothing about it, which is a known limit rather than a claim that the code is safe.
#[test]
fn a_return_inside_a_lambda_is_not_this_rules_business() {
    let codes = codes_of("void g() { auto l = []() -> int& { int x = 0; return x; }; }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::DanglingReturn,
        "the enclosing function stops at the lambda, so this is not its return",
    );
}

/// **A call result is not the local it was called on.**
///
/// Measured on `EmmyLuaCodeStyle`: a function returning `LuaStyle &` was reported for
///
/// ```cpp
/// std::shared_ptr<LuaEditorConfig> editorConfig = nullptr;
/// return editorConfig->Generate(fileUri);
/// ```
///
/// as "`editorConfig` is a local, and this function returns a reference to it". `Generate` returns a reference to the
/// object the pointer points at; whether that object outlives the call is a question about `Generate`'s body and the
/// object's owner, which this layer cannot answer — so the shape is silent. The members whose contract *is* "a view of
/// me" (`c_str`, `data`, `front`, …) still carry their receiver's origin, and a local returned directly is still
/// reported.
#[test]
fn a_member_call_on_a_local_is_not_the_local() {
    let codes = codes_of(
        "\
struct Config { Style &Generate(int uri); };
Style &f() {
    Config *config = nullptr;
    return config->Generate(1);
}
",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::DanglingReturn,
        "a call returns its own value, whatever type its receiver has",
    );

    let direct = codes_of("Style &g() {\n    Style local;\n    return local;\n}\n");
    assert_reported(
        &direct,
        DiagnosticCode::DanglingReturn,
        "a local returned directly is still the finding this rule exists for",
    );
}
