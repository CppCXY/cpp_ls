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
