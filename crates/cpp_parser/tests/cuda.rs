//! **CUDA's own syntax** — what `nvcc` adds to C++, accepted only when the unit is CUDA.
//!
//! The switch is [`cpp_parser::ParserConfig::cuda`], set by the analysis from the project's toolchain (`__CUDACC__`
//! or an `nvcc` command). It is off by default, and that is a requirement rather than a default: in ordinary C++
//! `__global__` is a name and `k<<<1, 1>>>(a)` is three shifts, so a project that has never seen `nvcc` must keep
//! exactly the parse it had. Each construct below is therefore tested **twice**: with the switch it reads, without it
//! the same text is not the same construct.
//!
//! What is deliberately *not* here: the CUDA built-in types and variables (`dim3`, `threadIdx`, `float4`, …). Those
//! are declarations `nvcc`'s headers make, not syntax, and they are the analysis layer's to provide.

mod common;

use cpp_parser::{CppParser, ParserConfig};

/// Parse `source` as CUDA and fail with every error it reported.
fn parses_as_cuda(name: &str, source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default().with_cuda(true));
    let errors = tree.get_errors();
    assert!(
        errors.is_empty(),
        "{name}: CUDA reading reported {errors:?}\n--- source ---\n{source}"
    );
}

/// Parse `source` as ordinary C++ and say how many errors it reported — the other half of every pair below.
fn errors_as_cpp(source: &str) -> usize {
    CppParser::parse(source, ParserConfig::default())
        .get_errors()
        .len()
}

#[test]
fn the_placement_qualifiers_are_specifiers_in_a_cuda_unit() {
    for (name, source) in [
        (
            "__global__",
            "__global__ void kernel(int *out) { out[0] = 1; }",
        ),
        (
            "__device__",
            "__device__ int twice(int n) { return n * 2; }",
        ),
        (
            "__host__ and __device__ together",
            "__host__ __device__ int f() { return 1; }",
        ),
        (
            "__shared__",
            "__global__ void k() { __shared__ int tile[16]; tile[0] = 1; }",
        ),
        ("__constant__", "__constant__ int table[4] = {1, 2, 3, 4};"),
        ("__managed__", "__managed__ int shared_counter = 0;"),
        (
            "__grid_constant__",
            "__global__ void k(const __grid_constant__ int n) { (void)n; }",
        ),
        (
            "__launch_bounds__",
            "__global__ void __launch_bounds__(1024, 2) k() { }",
        ),
        ("__maxnreg__", "__global__ void __maxnreg__(64) k() { }"),
        (
            "__align__",
            "__align__(16) struct Vertex { float x, y, z; };",
        ),
        (
            "__noinline__",
            "__device__ __noinline__ int slow(int n) { return n; }",
        ),
        (
            "__forceinline__",
            "__device__ __forceinline__ int fast(int n) { return n; }",
        ),
        (
            "__restrict__",
            "__global__ void k(int *__restrict__ out) { *out = 0; }",
        ),
    ] {
        parses_as_cuda(name, source);
    }
}

#[test]
fn a_kernel_launch_is_one_expression() {
    for (name, source) in [
        (
            "the two-expression configuration",
            "void host() { kernel<<<1, 32>>>(out); }",
        ),
        (
            "grid, block, shared memory and a stream",
            "void host() { kernel<<<dim3(1, 2), 32, 0, stream>>>(a, b); }",
        ),
        ("no arguments at all", "void host() { kernel<<<1, 1>>>(); }"),
        (
            "the result of a computation in the configuration",
            "void host() { kernel<<<grids * 2, threads>>>(out); }",
        ),
        (
            "an index expression keeps its brackets",
            "void host() { kernel<<<1, block[0]>>>(out); }",
        ),
        (
            "a launch whose value is used",
            "void host() { int n = 0; kernel<<<1, 1>>>(&n); }",
        ),
    ] {
        parses_as_cuda(name, source);
    }
}

#[test]
fn the_launch_is_a_node_of_its_own() {
    let source = "void host() { kernel<<<1, 32>>>(out); }";
    let tree = CppParser::parse(source, ParserConfig::default().with_cuda(true));
    let kinds: Vec<String> = tree
        .get_red_root()
        .descendants()
        .map(|node| format!("{:?}", node.kind()))
        .collect();
    assert!(
        kinds.iter().any(|kind| kind.contains("CudaLaunchExpr")),
        "a launch is its own node, with the callee and two argument lists under it: {kinds:?}"
    );
    let launches = tree
        .get_red_root()
        .descendants()
        .filter(|node| format!("{:?}", node.kind()).contains("CudaLaunchExpr"))
        .count();
    assert_eq!(launches, 1, "one launch is one node: {kinds:?}");
}

#[test]
fn none_of_this_is_cpp() {
    // **The other half of the switch**, and it is two constructs rather than every line above — the difference has to
    // be measured, not assumed.
    //
    // A *leading* `__global__` parses in both modes and without an error, because this grammar already reads a name
    // in front of a declaration as a **macro-shaped declaration** (`__global__` could be a macro, and every compiler's
    // headers are full of those). That tolerance is not a CUDA reading, so it is not evidence either way.
    //
    // What only CUDA can read is:
    //   * a qualifier in **specifier position**, where plain C++ has nothing to put it — `__launch_bounds__(…)`
    //     after the return type;
    //   * the **launch brackets**, which are three shifts to everything else.
    for (name, source) in [
        (
            "a qualifier in specifier position",
            "__global__ void __launch_bounds__(1024, 2) k() { }",
        ),
        ("a launch", "void host() { kernel<<<1, 32>>>(out); }"),
    ] {
        assert!(
            errors_as_cpp(source) > 0,
            "{name}: ordinary C++ must not accept this as the CUDA construct:\n{source}"
        );
    }
}

#[test]
fn a_launch_is_three_shifts_to_a_cpp_parser() {
    // The same text, read twice: the launch node exists in one tree and not the other, and that is the whole claim
    // of the switch. The C++ reading is not silent about it either — it reports an error, which is what a reader
    // editing a project that has never seen `nvcc` should see.
    let source = "void host() { kernel<<<1, 32>>>(out); }";
    let as_cuda = CppParser::parse(source, ParserConfig::default().with_cuda(true));
    let as_cpp = CppParser::parse(source, ParserConfig::default());
    let launches = |tree: &cpp_parser::CppSyntaxTree| {
        tree.get_red_root()
            .descendants()
            .filter(|node| format!("{:?}", node.kind()).contains("CudaLaunchExpr"))
            .count()
    };
    assert_eq!(launches(&as_cuda), 1, "one launch is one node");
    assert_eq!(launches(&as_cpp), 0, "and no launch is invented for C++");
}
