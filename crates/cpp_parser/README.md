# cpp_parser

[![crates.io](https://img.shields.io/crates/v/cpp_parser.svg)](https://crates.io/crates/cpp_parser)
[![docs.rs](https://img.shields.io/docsrs/cpp_parser.svg)](https://docs.rs/cpp_parser)

**A lossless C and C++ parser over preprocessed text** — a [rowan](https://github.com/rust-analyzer/rowan) CST with
error recovery, the tree every layer above this one reads. No semantic analysis, no name resolution, no opinion about
what a declaration means: a token in, the same token out.

```rust
use cpp_parser::{CppParser, ParserConfig, CppTranslationUnit};

let tree = CppParser::parse("struct S { int n; };", ParserConfig::default());
let unit = CppTranslationUnit::cast(tree.get_red_root()).expect("a translation unit");
assert!(tree.get_errors().is_empty());
```

## What it reads

| Standard | Constructs |
| --- | --- |
| C++11 … C++23 | as the standards define them — concepts, modules, coroutines, deducing `this`, `if consteval`, multidimensional subscript, `<=>`, `using enum`, `#elifdef` |
| **C++26** | **static reflection** (`^^T`, `[: r :]`, the three splice positions) and **contracts** (`pre(…)`, `post(…)`, `contract_assert(…)`) — parsed without a feature bit, because no earlier standard gives those tokens a meaning |
| C23 | `typeof`/`typeof_unqual`, `constexpr`, `nullptr`, `#elifdef` |
| CUDA | `__global__`/`__device__`/`__host__`, `<<<…>>>` launches, built-in vector types |
| GNU / MSVC / Clang | statement expressions, `__typeof__`, `__attribute__`, `__declspec`, `__try`/`__except`, `_STD_BEGIN`-shaped macro prefixes |

The construct-by-construct table, with the compiler-verified results, is
[`docs/cpp-support.md`](../../docs/cpp-support.md) and [`docs/c-support.md`](../../docs/c-support.md).

## What it does *not* do

- **No macro expansion.** The parser is handed text a preprocessor has already produced. `cpp_toolchain` is the
  preprocessor; a name being a macro is not a question this crate answers, and `--body`/`--macro` were removed from
  the dump tool for exactly that reason.
- **No semantics.** `Type`, overloads, members and scopes belong to `cpp_code_analysis`. This crate offers *shapes*:
  typed nodes over the CST, and predicates about those shapes (`CppDeclarator::get_param_list`, `is_a_pointer`, …).
- **No incremental reparse.** A tree is of one text. The cache above re-reads a file when it changes.

## The map

```text
lexer         tokens, trivia, and documentation comments (`///`, `//!`, `/**`, `/*!`) as a vocabulary of their own
parser        the event stream: checkpoints, markers, `complete`/`undo`, and `EventStreamAudit` for the invariants
grammar       the rules — declarations, declarators, specifiers, expressions, templates, classes, modules, attributes
syntax        the typed layer over the CST: one node type per construct, and the accessors a reader asks
symbols       macro bodies and environments by position: what a macro expands to, and where it is in force
parser_error  the diagnostics a reader sees, with a kind per constructor
text          `LineIndex`, `Reader`, `SourceRange` — byte offsets and the conversions between them
kind          the syntax kinds and token kinds: one enum for the tree, one for the lexer
probes        the tracing switches this crate answers to
```

Every node is typed: `CppDeclaration`, `CppDeclarator`, `CppParamList`, `CppClassDef`, … and each accessor is a
question about a shape rather than a walk of the text. The two rules those accessors follow are recorded where they
matter, e.g. [`docs/declarator-shapes.md`](../../docs/declarator-shapes.md) for why a declarator keeps its own
suffixes.

## Two invariants worth knowing before changing the grammar

1. **Lossless.** Every token the lexer produced appears in the tree, in order, with its text unchanged —
   `tests/lossless.rs` and `tests/invariants.rs` pin it. A grammar rule that consumes a token without attaching it
   fails those tests rather than a corpus.
2. **The version is part of every cache key.** [`VERSION`](src/lib.rs) re-exports `CARGO_PKG_VERSION` in one place,
   because a summary written by another grammar is a wrong answer, not a missing one.

## Testing

```bash
cargo test -p cpp_parser                 # the suites below
cargo run -p cpp_parser --bin cpp_dump -- file.cpp --tree     # the tree of a preprocessed file, errors included
```

```text
per standard      cpp11 cpp14 cpp17 cpp20 cpp23 reflection contracts
extensions        extensions_gnu extensions_msvc extensions_clang cuda
shape             ast declarator_shapes invariants lossless token_range designators direct_init
the rest          lexer doc doc_lexer doc_shape doc_owner symbols macros parse_cost modern gaps operators features
```

`tests/gaps.rs` is the honest list: constructs the grammar knows it does not read yet, one test per gap, each naming
the corpus line that found it.

## License

MIT — see [LICENSE](../../LICENSE).
