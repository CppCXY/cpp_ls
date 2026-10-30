//! **`dangling-return`**: a reference, a pointer or a view of something that dies when the function returns.
//!
//! Every fixture here is written without `#include`: the rule reads the **return type's spelling** and the returned
//! expression's **origin**, and neither needs a header. The project's own includes would be unresolved in a memory
//! file, which refuses the summary, and then no check runs at all.

use cpp_code_analysis::{
    CompilerConfig, DiagnosticCode, Finding, MemoryFiles, OpenDocuments, Session, SessionFiles,
    WatchFilter,
};

/// Every check finding for one fixture.
fn findings_of(source: &str) -> Vec<Finding> {
    let path = "/p/lifetime.cpp";
    let files = MemoryFiles::new().with_file(path, source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from(path)]);
    session.want_cooked_reading(std::path::Path::new(path));
    session.index_everything();
    session
        .diagnostics(std::path::Path::new(path))
        .map(|found| found.checks)
        .unwrap_or_default()
}

/// Only the `dangling-return` messages.
fn dangling(source: &str) -> Vec<String> {
    findings_of(source)
        .into_iter()
        .filter(|finding| finding.code == DiagnosticCode::DanglingReturn)
        .map(|finding| finding.message)
        .collect()
}

fn assert_one(source: &str, expected: &str) {
    let messages = dangling(source);
    assert_eq!(messages.len(), 1, "one finding expected: {messages:?}");
    assert!(
        messages[0].contains(expected),
        "`{expected}` missing from `{}`",
        messages[0]
    );
}

#[test]
fn a_function_returns_a_reference_to_its_own_local() {
    assert_one("int& f() { int x = 0; return x; }", "is a local");
}

#[test]
fn a_view_outlives_the_string_it_refers_to() {
    assert_one(
        "std::string_view f() { std::string s; return s; }",
        "refers to `s`",
    );
}

#[test]
fn a_reference_to_a_temporary_is_reported() {
    assert_one(
        "const std::string& f() { return std::string(\"x\"); }",
        "temporary",
    );
}

#[test]
fn a_pointer_into_a_local_is_reported() {
    assert_one(
        "const char* f() { std::string s; return s.c_str(); }",
        "`c_str()` returns a pointer into `s`",
    );
}

#[test]
fn the_address_of_a_local_is_reported() {
    assert_one("int* f() { int x = 0; return &x; }", "address of `&x`");
}

#[test]
fn a_local_in_an_inner_block_is_still_the_functions_own() {
    assert_one("int& f() { { int x = 0; return x; } }", "is a local");
}

#[test]
fn a_span_of_a_local_vector_is_reported() {
    assert_one(
        "std::span<int> f() { std::vector<int> v; return v; }",
        "refers to `v`",
    );
}

#[test]
fn a_moved_local_is_still_the_functions_own() {
    assert_one(
        "std::string_view f() { std::string s; return std::move(s); }",
        "refers to `s`",
    );
}

/// Only the `dangling-field` messages.
fn dangling_fields(source: &str) -> Vec<String> {
    findings_of(source)
        .into_iter()
        .filter(|finding| finding.code == DiagnosticCode::DanglingField)
        .map(|finding| finding.message)
        .collect()
}

/// **A view member of a by-value parameter**, which the constructor destroys before the object exists.
#[test]
fn a_view_member_of_a_by_value_parameter_is_reported() {
    let messages = dangling_fields("struct S { std::string_view v; S(std::string s) : v(s) {} };");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("parameter"), "{}", messages[0]);
}

/// **A view member of a temporary**, which dies at the end of the statement that built the object.
#[test]
fn a_view_member_of_a_temporary_is_reported() {
    let messages = dangling_fields("struct S { std::string_view v = std::string(\"x\"); };");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("temporary"), "{}", messages[0]);
}

/// A reference member is the same defect as a view member.
#[test]
fn a_reference_member_of_a_temporary_is_reported() {
    let messages =
        dangling_fields("struct S { const std::string& v; S() : v(std::string(\"x\")) {} };");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("reference"), "{}", messages[0]);
}

/// A pointer member from `s.c_str()`: the pointer refers to storage the parameter owns.
#[test]
fn a_pointer_member_of_a_parameters_buffer_is_reported() {
    let messages =
        dangling_fields("struct S { const char* p; S(std::string s) : p(s.c_str()) {} };");
    assert_eq!(messages.len(), 1, "{messages:?}");
}

/// A `span` member of a by-value vector is a view of a copy that dies with the constructor.
#[test]
fn a_span_member_of_a_by_value_vector_is_reported() {
    let messages =
        dangling_fields("struct S { std::span<int> v; S(std::vector<int> vec) : v(vec) {} };");
    assert_eq!(messages.len(), 1, "{messages:?}");
}

/// **The classes that are written correctly, and must stay quiet.** Each one is a shape a careless rule would report:
/// an owning member, a reference parameter (whose caller owns the lifetime), a member of `*this`, a literal, a
/// pointer whose target is unknown.
#[test]
fn the_safe_field_shapes_are_silent() {
    let safe = [
        // The member owns its storage: there is nothing to dangle.
        "struct S { std::string v; S(std::string s) : v(s) {} };",
        // A reference parameter lives as long as its argument — the caller's obligation, written down as
        // `[[clang::lifetimebound]]`.
        "struct S { std::string_view v; S(const std::string& s) : v(s) {} };",
        "struct S { std::string_view v; S(std::string&& s) : v(s) {} };",
        // A member of the object outlives the constructor, and a static outlives everything.
        "struct S { std::string s; std::string_view v; S() : v(s) {} };",
        "struct S { static const std::string name; std::string_view v; S() : v(name) {} };",
        // A string literal has static storage duration.
        "struct S { std::string_view v; S() : v(\"literal\") {} };",
        "struct S { std::string_view v = \"literal\"; };",
        // A pointer whose target is not known.
        "struct S { int* p; S(int* q) : p(q) {} };",
        // A member with no initialiser borrows nothing.
        "struct S { std::string_view v; S() {} };",
    ];
    for source in safe {
        let messages = dangling_fields(source);
        assert!(messages.is_empty(), "`{source}` reported {messages:?}");
    }
}

/// **The declarations that are written correctly, and must stay quiet.** Each one is a shape a careless rule would
/// report: a value return, a parameter (whose caller owns the lifetime), a static, a literal, a member.
#[test]
fn the_safe_shapes_are_silent() {
    let safe = [
        // The result is an owner: no reference outlives the call.
        "std::string f() { std::string s; return s; }",
        // A parameter lives as long as its argument — the caller's rule, not this function's.
        "int& f(int& x) { return x; }",
        "std::string_view f(const std::string& s) { return s; }",
        "const std::string& f(const std::string& s) { return s; }",
        "struct S { std::string name; }; std::string_view f(const S& s) { return s.name; }",
        // Static storage outlives every call.
        "int& f() { static int x = 0; return x; }",
        "std::string_view f() { static std::string s; return s; }",
        "const char* f() { static const char* p = \"x\"; return p; }",
        // A string literal is an array with static storage duration.
        "std::string_view f() { return \"literal\"; }",
        // A pointer's target is not known: `p` may point at a global, at the heap, or at nothing.
        "int* f() { return nullptr; }",
        "int* p = nullptr; int& f() { return *p; }",
        // `*this`'s member outlives the call by the caller's own obligation.
        "struct S { std::string name; std::string_view get() { return name; } };",
        // A call whose body nobody can see is not evidence.
        "int& f(); int& g() { return f(); }",
    ];
    for source in safe {
        let messages = dangling(source);
        assert!(messages.is_empty(), "`{source}` reported {messages:?}");
    }
}

/// **A return inside a lambda belongs to the lambda**, not to the function the lambda is written in — and this rule
/// does not analyse lambdas yet. The fixture is a known limit, not a claim that the code is safe.
#[test]
fn a_lambda_return_is_not_this_rules_business() {
    assert!(dangling("void g() { auto l = []() -> int& { int x = 0; return x; }; }").is_empty());
}

/// **One finding per return.** The enclosing function is found by walking outward, and a walk that started from the
/// body would report the same statement once per enclosing block.
#[test]
fn a_return_is_reported_once() {
    assert_eq!(dangling("int& f() { int x = 0; return x; }").len(), 1);
}
