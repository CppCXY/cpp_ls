//! **Ascend C's own syntax** — the AICore qualifiers Huawei's kernel language adds to C++.
//!
//! The switch is [`cpp_parser::ParserConfig::ascend_c`], set by the project family. In ordinary C++ the same words
//! are identifiers: `__global__ __aicore__ void kernel(...)` is a declaration of a variable `__global__` followed by
//! a declaration of `__aicore__`, and the kernel body is not a function body. Each construct is therefore tested with
//! the switch on, and the qualifier spellings are not accepted without it.

use cpp_parser::{CppParser, ParserConfig, ProjectLanguage};

fn parses_as_ascend(name: &str, source: &str) {
    let tree = CppParser::parse(
        source,
        ParserConfig::default().with_language_family(ProjectLanguage::AscendC),
    );
    let errors = tree.get_errors();
    assert!(
        errors.is_empty(),
        "{name}: Ascend C reading reported {errors:?}\n--- source ---\n{source}"
    );
}

#[test]
fn the_aicore_qualifiers_are_specifiers_in_an_ascend_unit() {
    for (name, source) in [
        (
            "global and aicore",
            "__global__ __aicore__ void add_kernel(__gm__ uint8_t *x, __gm__ uint8_t *y) { }",
        ),
        (
            "aicore only",
            "__aicore__ inline int twice(int n) { return n * 2; }",
        ),
        (
            "memory spaces",
            "__gm__ half *global_ptr; __ubuf__ half local[16]; __cbuf__ half cube[16];",
        ),
        (
            "host and device",
            "__host__ int host_value; __device__ int device_value;",
        ),
        (
            "extern C kernel",
            "extern \"C\" __global__ __aicore__ void kernel(__gm__ float *out) { out[0] = 1.0f; }",
        ),
    ] {
        parses_as_ascend(name, source);
    }
}

#[test]
fn a_kernel_at_the_start_of_a_unit_is_a_declaration() {
    let source = "__global__ __aicore__ void bad() { new int; }\n";
    let tree = CppParser::parse(
        source,
        ParserConfig::default().with_language_family(ProjectLanguage::AscendC),
    );
    assert!(tree.get_errors().is_empty(), "{:?}", tree.get_errors());
    let kinds: Vec<String> = tree
        .get_red_root()
        .descendants()
        .map(|node| format!("{:?}", node.kind()))
        .collect();
    assert!(
        kinds.iter().any(|kind| kind.contains("Declaration")),
        "the qualifiers begin a declaration: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| kind.contains("MacroCall")),
        "the qualifiers are not a macro call: {kinds:?}"
    );
}

#[test]
fn an_ascend_kernel_launch_is_one_expression() {
    // Ascend C's host launch has the same `<<<…>>>` shape as CUDA's: `kernel<<<blockDim, nullptr, stream>>>(args)`.
    // It is not C++, so the family is what makes it a launch node rather than three shifts.
    let source = "\
void host() {
    add_kernel<<<8, nullptr, stream>>>(x, y, z);
}
";
    let tree = CppParser::parse(
        source,
        ParserConfig::default().with_language_family(ProjectLanguage::AscendC),
    );
    assert!(
        tree.get_errors().is_empty(),
        "the launch parses: {:?}",
        tree.get_errors()
    );
    let launches = tree
        .get_red_root()
        .descendants()
        .filter(|node| format!("{:?}", node.kind()).contains("CudaLaunchExpr"))
        .count();
    assert_eq!(launches, 1, "one launch is one node");
}

#[test]
fn the_ascend_c_vocabulary_is_ordinary_cpp() {
    // `AscendC::TPipe`, `TQue<…>` and `GlobalTensor<…>` are declarations from the toolkit's headers, not grammar.
    // This test is the line between what the parser adds (the reserved qualifiers above) and what the project's
    // headers provide.
    parses_as_ascend(
        "toolkit names",
        "\
namespace AscendC {
    struct TPipe { };
    template <typename T> struct GlobalTensor { };
    template <int Position, int Depth> struct TQue { };
}
void f() {
    AscendC::TPipe pipe;
    AscendC::GlobalTensor<float> tensor;
    AscendC::TQue<0, 1> queue;
}
",
    );
}
