# cpp_ls

[![Rust](https://github.com/CppCXY/cpp_ls/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/CppCXY/cpp_ls/actions/workflows/build.yml)
[![Test](https://github.com/CppCXY/cpp_ls/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/CppCXY/cpp_ls/actions/workflows/test.yml)
[![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls)
[![downloads](https://img.shields.io/crates/d/cpp_ls.svg)](https://crates.io/crates/cpp_ls)
[![docs.rs](https://img.shields.io/docsrs/cpp_ls.svg)](https://docs.rs/cpp_ls)
[![license](https://img.shields.io/crates/l/cpp_ls.svg)](#license)

**A C, C++ and CUDA language server built from scratch in Rust.** The parser, preprocessor, index, semantic
engine and LSP server are all in this repository. There is no clangd, no libclang and no compiler daemon underneath;
there is also no build system to configure before the first answer. A compilation database is used when a project has
one, because it makes the flags exact, but it is an optimization rather than a prerequisite.

```bash
cargo install cpp_ls
```

- [Why cpp_ls](#why-cpp_ls)
- [How it compares](#how-it-compares)
- [Compiler selection](#compiler-selection)
- [Features](#what-it-answers)
- [Getting started](#getting-started)
- [Crates](#crates)
- [Documentation](#documentation)

## Why cpp_ls

C++ is not read correctly one file at a time. What a file means depends on the includes before it, the conditionals
that were taken, the macros that were expanded and the language mode the compiler was invoked with. Most language
servers either ask a compiler to do that work or approximate it. cpp_ls does it itself, which leads to five design
choices.

### 1. The program is the unit of work

A file is rendered the way a compiler would see it: includes inlined, the active `#if` branch selected, macros
expanded, and modules resolved from their source interfaces. The parser then reads the resulting program, not a
disconnected buffer. A typical `import std;` translation unit can be a **384-file, 4.85 MB rendering** parsed once and
shared by every file that imports the module.

### 2. Modules do not need BMIs or PCMs

C++20 modules are handled at the source level. `import std;` reads the implementation's own module source —
libstdc++'s `bits/std.cc`, MSVC's `modules/std.ixx`, libc++'s `std.cppm` — and a project's units are found by reading
what each candidate actually declares (`export module geometry:shapes;`), not by guessing from a filename. Partitions,
re-exports and implementation units follow the language rules. No build artifact has to exist first.

### 3. No build system is required

A Visual Studio solution, an unconfigured CMake tree and a checkout with only a Makefile all work. Includes that no
compiler has resolved are found with a bounded search under the project root, and a project can describe itself in
`.cppls.toml` when heuristics are not enough.

When a build description **does** exist, cpp_ls uses it for exact per-file flags:

```text
compile_commands.json   CMake, Ninja, Meson, Bazel, Bear, … can all export it
build_database.json     CMake 3.31's module-aware format, same commands plus module info
```

Both are discovered in the workspace root or up to three directories down, `@response` files are followed, and
`[compile] database` names one explicitly when a project has several builds. See
[`docs/configuration.md`](docs/configuration.md#the-two-database-formats).

### 4. It is built for code being edited

A completion is requested while a line is half-written. The parser keeps error recovery, the analysis keeps answering
from the text that exists, and the editor receives `isIncomplete` rather than an excuse when a reading is still being
built. The pipeline is asynchronous: an edit is acknowledged and applied by one writer, while read-only queries use the
current snapshot.

### 5. The claims are measured

The workspace has **3,400+ tests**, including integration tests against real standard libraries and real projects.
The feature tables in [`docs/cpp-support.md`](docs/cpp-support.md) and [`docs/c-support.md`](docs/c-support.md) are
construct-by-construct records rather than a feature wishlist.

## How it compares

The table compares clangd, clice and cpp_ls along the dimensions that decide how a C++ language server behaves: where
the language semantics come from, what a project must provide, and what runs while a query is answered.

[clangd](https://clangd.llvm.org/) is the LLVM project's C/C++ language server. [clice](https://clice.io/) is a
newer language server built on LLVM/Clang, with multi-process compilation, C++20 modules, header contexts and template
pseudo-instantiation; its feature documentation is at
[docs.clice.io](https://docs.clice.io/clice/features/overview) and its source is
[clice-io/clice](https://github.com/clice-io/clice). cpp_ls is the self-contained implementation in this repository.

| | clangd | clice | cpp_ls |
| --- | --- | --- | --- |
| Front end | LLVM/Clang | LLVM/Clang | a C++ parser, preprocessor and type engine written in Rust |
| Project input | a compilation database, or fallback flags | a compilation database is the normal input | optional: `.cppls.toml`, a compilation database, or neither |
| Compiler and headers | the compile command's compiler; Clang's driver resolves its headers | the compile command's compiler, with clice's toolchain detection | discovers the project's compiler (or the system default) and asks it for search paths and predefined macros |
| Unit of analysis | a file and its compile command, as a Clang AST | a file and its compilation context, as a Clang AST | the preprocessed program: includes inlined, active `#if` branches taken, macros expanded |
| Language surface | C, C++, CUDA and Objective-C/C++ through Clang | C, C++, CUDA, HIP, OpenCL; Objective-C/C++ for reading | C, C++, CUDA and Ascend C |
| Language standards | whatever the pinned Clang accepts | whatever the pinned Clang accepts | C++11 through C++26, including static reflection (`^^T`, `[: r :]`) and contracts (`pre`/`post`/`contract_assert`), plus C23 and common GNU/MSVC extensions |
| C++20 modules | Clang's module support; the build produces the artifacts | builds PCHs/PCMs and caches them on disk | reads module source interfaces; no BMI or PCM to build |
| Diagnostics | Clang, with clang-tidy integration | Clang and clang-tidy-shaped analysis | its own diagnostic registry, with each rule switchable by name |
| Templates | Clang instantiation | Clang instantiation plus a DFS pseudo-instantiator | its own instantiation and dependent-name analysis |
| Execution | one process with worker threads | a master process and isolated worker processes | one Rust server process; no compiler process in the query path |
| Extra tooling | clang-tidy, clang-format, background index | `clice` CLI: index, query, refactor, analyze, modularize, lint, format | `cpp_check` and `inspect`, plus the optional `cpp_formatter` |
| License | Apache-2.0 WITH LLVM-exception | Apache-2.0 | MIT |

The comparison is about design boundaries, not a benchmark.

### What cpp_ls does not claim

- It is **not Clang**. Its parser and type engine cover the language and the common library shapes, but compiler-exact
  overload resolution and heavy template metaprogramming are outside its scope.
- CUDA device-side semantics are not modelled. cpp_ls reads `.cu` files and the toolkit's headers; `nvcc` remains the
  compiler.
- The Ascend checks are editor checks for Ascend C kernels, not a replacement for the CANN compiler.
- The formatter is a fallback, not a byte-compatible clang-format.
- Only the first workspace folder is analyzed.

## Compiler selection

cpp_ls does not ship its own copy of the standard library. The headers a file sees belong to the compiler that builds
it: libstdc++ and libc++ differ, MSVC's STL is different again, and a compiler also carries its own target, sysroot and
predefined macros. Hard-coding one set would make the analysis describe headers the project does not use.

So cpp_ls uses the project's compiler as a source of configuration, not as the parser. It asks the compiler where its
headers are (`-E -v`), what it predefines (`-dM`) and what version it is (`--version`); those answers become the
include search path and macro environment for the project. No compiler process runs during an ordinary query, and the
answers are cached under the project cache for the next open.

The compiler is chosen in this order:

1. **`.cppls.toml`**: `[compile] compiler = "clang++"`. `"auto"` and an absent key mean "use the discovery below".
2. **The project's compilation database**: the compiler named by `compile_commands.json` or `build_database.json`.
3. **The environment**: `CXX`, then `CC`.
4. **The platform's toolchain**: MSVC on Windows, when the project and the environment named nothing.
5. **An installed compiler**: the conventional names and locations an installer uses (`clang++`, `g++`, …).
6. **`PATH`**: the compiler names the shell can see, with `g++`, `clang++`, `c++`, `gcc`, `clang` and `cc` as
   candidates.
7. **System header directories**: when no compiler can be asked at all, the conventional directories are used, without
   a compiler's predefined macros.

A candidate that is missing or does not answer is skipped, not fatal; the next one is tried. The selected
`ToolchainSource` is recorded, so a project can tell whether the headers in use came from its own compiler, `CXX`, the
platform, an installation or `PATH`.

The order puts the project's own statements first because those are the only answers that describe *this* project.
`PATH` is last because it describes one shell, not necessarily the compiler that built the project.

## What it answers

### Reading

| Feature | What it does |
| --- | --- |
| Completion | Members after `.`, `->` or `::`, through pointers, smart pointers, bases and instantiated templates; names in scope; Doxygen commands and symbol references inside documentation comments |
| Hover | The declaration, its deduced type and its documentation, rendered as a card |
| Signature help | The parameter list being filled in, from the same facts the call is checked against |
| Definition, type definition, implementation | Where a name, its type or the override of a virtual function is written; a documentation reference jumps to the declaration |
| References, rename, document highlight | Macro uses exactly (the macro environment decides each one), and symbols through the index |
| Call hierarchy, document and workspace symbols | Who calls what, and the shape of a file or a project |
| Folding and selection ranges | Blocks, comments and preprocessor regions; the syntactic unit around the cursor |
| Format strings | Completion inside `printf`, `std::format` and `fmt::format` strings; hover on a specifier; four format-specific semantic-token types |

### Editing and checking

- **Inlay hints** — the deduced type of `auto` and structured bindings, and parameter names at a call.
- **CodeLens** — a function's reference count on its declaration and definition. Counts are resolved lazily: the first
  request returns anchors, and the client asks for a count only for the lenses it actually draws.
- **Semantic tokens** — names, types, keywords, documentation commands, symbol references and format-string segments
  (`formatSpecifier`, `formatArgument`, `formatConversion`, `formatEscape`).
- **Code actions** — quick fixes from the same diagnostics.
- **Diagnostics** — `textDocument/diagnostic` for pull clients and versioned
  `textDocument/publishDiagnostics` otherwise. The registry is switchable by name in `.cppls.toml`: syntax errors,
  undefined names and fields, argument counts and types, return mismatches, access, concepts, missing returns, lifetime
  rules, the override/`final` family, Unreal checks, style rules, and notes that are not complaints at all. See
  [`docs/cpp-support.md` §6](docs/cpp-support.md#6-diagnostics) and
  [`docs/c-support.md` §6](docs/c-support.md#6-diagnostics).

### Documentation comments

A documentation comment is a language of its own, not prose the analysis skips. `///`, `//!`, `/**` and `/*!` are all
understood. Doxygen commands are completed and explained; `@param` completes the declaration's parameters; `@see`,
`@ref`, `@sa`, `@overload` and the other reference commands complete visible names; and Rust-style
`` [`Widget::grow`] `` references get the declaration's colour, card and definition target. `@code` blocks are read as
code, line by line, with the comment marker removed. The full contract is in
[`docs/cpp-support.md`](docs/cpp-support.md#5-lsp-features).

### C, CUDA and Ascend

C is a first-class dialect, not a C++ mode with checks disabled. CUDA is a real toolchain case: `.cu` and `.cuh` are
read with `<<<…>>>` launch blocks and CUDA keywords, and a `[cuda]` section points the analysis at the toolkit's
headers. Device-side semantic restrictions are not modelled; cpp_ls is an editor's front end, not a replacement for
`nvcc`.

Ascend C kernels are supported as their own dialect: CCE qualifiers and sections, CANN header discovery, and kernel
checks for device restrictions such as dynamic allocation and C++ exceptions. This is for editing Ascend code, not a
substitute for the CANN compiler.

### Formatting

[`cpp_formatter`](crates/cpp_formatter) is an independent implementation that imitates clang-format — not a port and
not the same program. It reads the nearest `.clang-format` and answers `textDocument/formatting`,
`textDocument/rangeFormatting` and `textDocument/willSaveWaitUntil`, so a bare install formats without clang-format on
the machine. Its output is idiomatic, not byte-identical to clang-format's.

**For production formatting, use clang-format.** cpp_ls's formatter is the fallback that makes a fresh install usable.

## Getting started

```bash
cargo install cpp_ls
# or build the workspace:
cargo build --release -p cpp_ls

cpp_ls --help          # stdio by default; also --tcp, --log-level, --config
```

Point an editor at `cpp_ls` on `PATH` (or `target/release/cpp_ls` when building from source). The VS Code client is
[CppCXY/VSCode-Cppls](https://github.com/CppCXY/VSCode-Cppls); any other LSP client works directly.

A project needs no configuration. When it wants some, a `.cppls.toml` in the workspace root names the language
standard, include paths, defines, per-path rules and diagnostic switches. A **compilation database** supplies exact
flags when one exists: `compile_commands.json` from any build system, or CMake 3.31's module-aware
`build_database.json` (`CMAKE_EXPORT_BUILD_DATABASE`). Both are found in the root or up to three directories down;
`[compile] database` names one explicitly. See
[`docs/configuration.md`](docs/configuration.md#the-two-database-formats).

The server currently analyzes the first workspace folder only. If the client cannot dynamically register file watches,
a native watcher monitors that folder, and watcher failures are reported to the client.

## Crates

| Crate | crates.io | What it is |
| --- | --- | --- |
| [`cpp_parser`](crates/cpp_parser) | [![crates.io](https://img.shields.io/crates/v/cpp_parser.svg)](https://crates.io/crates/cpp_parser) | A lossless C++ parser over preprocessed text: a rowan CST with error recovery, C++11 through C++26, C23, CUDA and vendor extensions |
| [`cpp_toolchain`](crates/cpp_toolchain) | [![crates.io](https://img.shields.io/crates/v/cpp_toolchain.svg)](https://crates.io/crates/cpp_toolchain) | The build description, compiler discovery, include resolution, translation-unit walk and preprocessor |
| [`cpp_code_analysis`](crates/cpp_code_analysis) | [![crates.io](https://img.shields.io/crates/v/cpp_code_analysis.svg)](https://crates.io/crates/cpp_code_analysis) | The index and its cache, scopes, type inference, sessions, completions, modules and diagnostics |
| [`cpp_ls`](crates/cpp_ls) | [![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls) | The LSP transport, handlers, configuration and the server binary |
| [`cpp_formatter`](crates/cpp_formatter) | [![crates.io](https://img.shields.io/crates/v/cpp_formatter.svg)](https://crates.io/crates/cpp_formatter) | A formatter with clang-format's configuration surface |

Each crate has its own README: [`cpp_parser`](crates/cpp_parser#readme) · [`cpp_toolchain`](crates/cpp_toolchain#readme) ·
[`cpp_code_analysis`](crates/cpp_code_analysis#readme) · [`cpp_ls`](crates/cpp_ls#readme) ·
[`cpp_formatter`](crates/cpp_formatter#readme) · [`cpp_check`](crates/cpp_check#readme) — the project-wide front end for
the analysis's checks (`publish = false`).

## Working on it

```bash
cargo test --workspace --release          # 3,400+ tests; latency tests only measure in --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

`inspect` asks a question without an editor:

```bash
inspect hover    --project . --file src/main.cpp --needle cout --shift 2
inspect define   --project . --file src/main.cpp --needle std::map
inspect members  --project . --file src/main.cpp --needle value --index 2
inspect facts    --fixture "a.cpp=struct S { int x; };" --file a.cpp
```

## Documentation

- [`docs/cpp-support.md`](docs/cpp-support.md) — what the analysis reads, answers and reports for C++, with the
  diagnostic registry and each rule's severity
- [`docs/c-support.md`](docs/c-support.md) — the same for C
- [`docs/configuration.md`](docs/configuration.md) — `.cppls.toml`, the compilation database, include paths and
  standards
- [`docs/cpp-check.md`](docs/cpp-check.md) — project-wide checks outside an editor

## AI assistance

DeepSeek was the primary AI model used for implementation. Other models, including Maicode2, Gemini 4, Claude 5.5
Sonnet and GPT-6.1, were also used for review and feedback. The final design and decisions remain with the maintainers.

## License

MIT. See [`LICENSE`](LICENSE).
