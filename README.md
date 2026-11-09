# cpp_ls

[![Rust](https://github.com/CppCXY/cpp_ls/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/CppCXY/cpp_ls/actions/workflows/build.yml)
[![Test](https://github.com/CppCXY/cpp_ls/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/CppCXY/cpp_ls/actions/workflows/test.yml)
[![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls)
[![downloads](https://img.shields.io/crates/d/cpp_ls.svg)](https://crates.io/crates/cpp_ls)
[![docs.rs](https://img.shields.io/docsrs/cpp_ls.svg)](https://docs.rs/cpp_ls)
[![license](https://img.shields.io/crates/l/cpp_ls.svg)](#license)

A C, C++ and CUDA language server written from scratch in Rust: the parser, the preprocessor, the index, the semantic
layer and the LSP server are all in this repository. **No clangd, no libclang, no compiler daemon, and no
`compile_commands.json` required.**

## Why another C++ language server

| | clangd | cpp_ls |
| --- | --- | --- |
| Project model | a compilation database, or heuristics | nothing, or `.cppls.toml`, or a compilation database |
| Unit of work | a file with flags | **the program**: includes inlined, `#if` resolved, macros expanded, modules cooked |
| Modules | needs BMIs produced by the build | reads the interface's **source** (`import std;` → GCC's `bits/std.cc`, MSVC's headers); partitions resolved by naming convention |
| Answers in | a file that compiles | a file being edited — half-typed lines included |
| Diagnostics | Clang's | its own 26 checks, measured at **zero false positives** on libstdc++'s own sources |

## What makes it different

**1. The program is the unit of work, not the file.** A C++ file does not mean what its characters say until a
preprocessor has run, so one runs: `#include`s are inlined, the `#if` branch actually in force is taken, macros are
expanded, and the parser reads the text a compiler would see. The closure of `import std;` on this machine is **384
files** and a **4.85 MB** rendering; every answer after that comes from **one** parse of it, shared by every file that
imports the module.

**2. Modules are cooked from source, partitions included.** `import std;` is answered by resolving the module's
interface unit (GCC's `<include>/bits/std.cc`, MSVC's header tree), not by asking for a BMI the build never produced.
`export module geometry:shapes;` is resolved through the naming conventions projects actually use
(`<module>_<partition>.cppm`, `-`, `/`, `.`), an `export import :part;` re-exports that partition's names to importers,
and an implementation unit (`module geometry;`) sees them all.

**3. No build system required.** Includes that no compiler ever looked at are found by a bounded fuzzy search under the
project root, so a Visual Studio solution and an unconfigured checkout both work. A compilation database, when present,
is used for exact per-file flags.

**4. Every claim in this repository is measured.** **2,545 tests across 167 suites**, plus probes that run against real
headers rather than fixtures. [`docs/buglist.md`](docs/buglist.md) is a record of before/after numbers: a sweep of
libstdc++'s 2,500 headers, MSVC's 166 STL headers, and a construct-by-construct table of compiler extensions
(**GCC 40, Clang 29, MSVC 61**) all run in CI.

**5. Hover is reconstructed, never a text slice.** A hover card is built from the indexed fact — signature, deduced type
(`auto`, `decltype`, structured bindings), doc comment — and a type is answered by its **definition**, not by whichever
forward declaration happens to sit in `<format>`.

## Crates

| Crate | What it is |
| --- | --- |
| [`cpp_parser`](crates/cpp_parser) [![crates.io](https://img.shields.io/crates/v/cpp_parser.svg)](https://crates.io/crates/cpp_parser) [![docs.rs](https://img.shields.io/docsrs/cpp_parser.svg)](https://docs.rs/cpp_parser) | A lossless C++ parser over preprocessed text: a rowan CST with error recovery, C++23/C23 features, CUDA, and vendor extensions |
| [`cpp_code_analysis`](crates/cpp_code_analysis) [![crates.io](https://img.shields.io/crates/v/cpp_code_analysis.svg)](https://crates.io/crates/cpp_code_analysis) [![docs.rs](https://img.shields.io/docsrs/cpp_code_analysis.svg)](https://docs.rs/cpp_code_analysis) | The preprocessor and cook, the index and its cache, symbol scopes, type inference, modules/partitions, diagnostics |
| [`cpp_ls`](crates/cpp_ls) [![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls) [![docs.rs](https://img.shields.io/docsrs/cpp_ls.svg)](https://docs.rs/cpp_ls) | The server: LSP transport, handlers, configuration, and the binary |
| [`cpp_formatter`](crates/cpp_formatter) [![crates.io](https://img.shields.io/crates/v/cpp_formatter.svg)](https://crates.io/crates/cpp_formatter) [![docs.rs](https://img.shields.io/docsrs/cpp_formatter.svg)](https://docs.rs/cpp_formatter) | An independent formatter with clang-format's configuration surface |
| [`inspect`](tools/inspect) | A CLI that asks the same layers the server does — `hover`, `define`, `members`, `complete`, `facts`, `type`, `diagnostics`, `tree` — with no editor in the loop |

## What it answers

**Reading**

| Feature | Contents |
| --- | --- |
| Completion | Members after `.` / `->` / `::`, through pointers, smart pointers, bases and instantiated templates; names in scope |
| Hover | The declaration, the deduced type, and the doc comment (Doxygen commands included) |
| Signature help | The parameter list being filled in, from the same facts the call is checked against |
| Definition / type definition / implementation | Where a name, its type, or the override of a virtual function is written |
| References, rename, document highlight | Every use in the project, and a rename that edits exactly those |
| Call hierarchy, document and workspace symbols | Who calls what, and the shape of a file or a project |
| Folding, selection range | Blocks, comments and regions; the syntactic unit around the cursor |
| Format-string awareness | Completion inside `printf` / `std::format` / `fmt::format` strings, hover on a specifier, and four custom semantic-token types |

**Editing and checking**

- **Inlay hints** — the deduced type of `auto` and structured bindings, parameter names at a call.
- **Semantic tokens** — names, types, keywords, documentation commands, and format strings (`formatSpecifier`,
  `formatArgument`, `formatConversion`, `formatEscape`).
- **Code actions** — quick fixes from the same diagnostics.
- **Diagnostics** — pull-based (`textDocument/diagnostic`), 26 codes: `syntax-error`, `include-not-found`,
  `undefined-name`, `undefined-field`, `redefinition`, `missing-return`, `argument-count`, `param-type-check`,
  `assign-type-check`, `return-type-mismatch`, `access-invisible`, `concept-not-satisfied`,
  `assignment-in-condition`, `unnecessary-branch`, `macro-redefined`, nine clang-tidy-shaped style rules
  (`cstyle-cast`, `use-auto`, `use-nullptr`, …), `unused`, `module-note` — each one enableable by name in
  `.cppls.toml`.

**C and CUDA.** C is a first-class dialect. CUDA is a **special toolchain case**: `.cu` and `.cuh` are read with
`<<<…>>>` launch blocks and the CUDA keywords, and a `[cuda]` section tells the analysis where the toolkit's headers
are. What a compiler enforces about device code is not modelled.

## Getting started

```bash
cargo build --release -p cpp_ls          # one binary, LSP over stdio by default
target/release/cpp_ls --help             # tcp, log level, config path, standard-library switches
```

Point an editor at `target/release/cpp_ls`. The VS Code client is
[CppCXY/VSCode-Cppls](https://github.com/CppCXY/VSCode-Cppls); any other LSP client works directly.

A project needs no configuration. When it wants some, a `.cppls.toml` in the workspace root names the language standard,
include paths, defines, per-path rules and diagnostic switches; a `compile_commands.json` is used for exact flags when it
exists. Both are described in [`docs/configuration.md`](docs/configuration.md).

## Working on it

```bash
cargo test --workspace --release          # 2,545 tests; the latency ones only measure in --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

The probes are the interesting part: `CPPLS_TRACE_HOVER`, `CPPLS_TRACE_COOKING`, `CPPLS_TRACE_VISIBILITY`,
`CPPLS_TRACE_NAMES`, `CPPLS_TRACE_DECLARATION` and `CPPLS_DUMP_RENDERING_DIR` print what a layer decided and why, and
`cargo test --release -- --ignored --nocapture` runs the probes that need a real project or toolchain. `inspect` asks a
question without an editor:

```bash
inspect hover    --project . --file src/main.cpp --needle cout --shift 2
inspect define   --project . --file src/main.cpp --needle std::map
inspect members  --project . --file src/main.cpp --needle value --index 2
inspect facts    --fixture "a.cpp=struct S { int x; };" --file a.cpp
```

## Formatting

`cpp_formatter` is an **independent implementation that imitates clang-format** — not a port, and not the same program.
It reads the nearest `.clang-format` (`BasedOnStyle`, the seven presets, and the options it honours) and answers
`textDocument/formatting`, `rangeFormatting` and `willSaveWaitUntil`, so the server is usable with nothing else
installed. Its output is idiomatic, not byte-identical to clang-format's.

**For production code, use clang-format**; treat this as the fallback that makes a bare install work.

## Documentation

[`docs/cpp-support.md`](docs/cpp-support.md) · [`docs/c-support.md`](docs/c-support.md) ·
[`docs/configuration.md`](docs/configuration.md) · [`docs/locking.md`](docs/locking.md) ·
[`docs/buglist.md`](docs/buglist.md) — what works, how to configure it, how the cache is locked, and what is still open
with its measurement.

## License

MIT. See [`LICENSE`](LICENSE).

Implemented by DeepSeek.
