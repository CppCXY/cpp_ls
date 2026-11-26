//! What `ascend-dynamic-allocation`, `ascend-exception` and `ascend-std-type` say — and what they leave alone.
//!
//! The restriction is about **device code**: a host helper beside a kernel may allocate and use `std::vector`, and
//! an ordinary C++ file is not Ascend C at all.

use crate::ProjectLanguage;

use super::super::DiagnosticCode;
use super::{assert_reported, assert_silent_about, codes_in_family};

fn codes_of_ascend(source: &str) -> Vec<DiagnosticCode> {
    codes_in_family(
        &[("/p/a.cpp", source)],
        "/p/a.cpp",
        ProjectLanguage::AscendC,
    )
}

#[test]
fn a_kernel_may_not_allocate_except_or_use_the_standard_library() {
    let source = "\
__global__ __aicore__ void bad() {
    auto *p = new int[16];
    try { work(); } catch (...) { }
    std::vector<int> values;
}
";
    let codes = codes_of_ascend(source);
    assert_reported(
        &codes,
        DiagnosticCode::AscendDynamicAllocation,
        "`new` is not available on the AI Core",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AscendException,
        "exceptions are not available on the AI Core",
    );
    assert_reported(
        &codes,
        DiagnosticCode::AscendStdType,
        "`std::vector` is host-side storage",
    );
}

#[test]
fn a_host_helper_beside_the_kernel_is_not_device_code() {
    let source = "\
void host_allocates() {
    auto *p = new int[16];
    std::vector<int> values;
    try { work(); } catch (...) { }
}

__global__ __aicore__ void kernel() {
    compute();
}
";
    let codes = codes_of_ascend(source);
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendDynamicAllocation,
        "the allocation is in a host function",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendException,
        "the exception is in a host function",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendStdType,
        "the container is in a host function",
    );
}

#[test]
fn comments_and_strings_are_not_device_code() {
    // The point of reading the tree: `new` in a comment and `std::vector` in a string are not expressions or type
    // names, and neither may be reported.
    let source = "\
__global__ __aicore__ void kernel() {
    // new int[16]; std::vector<int>
    const char *note = \"new std::vector<int>\";
}
";
    let codes = codes_of_ascend(source);
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendDynamicAllocation,
        "the comment mentions new, the tree has no NewExpr",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendStdType,
        "the string mentions std::vector, the tree has no declaration",
    );
}

#[test]
fn a_non_ascend_project_is_silent() {
    let source = "__aicore__ void f() { auto *p = new int; }\n";
    let codes = codes_in_family(&[("/p/a.cpp", source)], "/p/a.cpp", ProjectLanguage::Cpp);
    assert_silent_about(
        &codes,
        DiagnosticCode::AscendDynamicAllocation,
        "a project that is not Ascend C has no AI Core kernel",
    );
}
