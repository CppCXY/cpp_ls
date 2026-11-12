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
| Unit of work | a file with flags | **the program**: includes inlined, `#if` resolved, macros expanded, modules resolved from their interface units |
| Modules | needs BMIs produced by the build | reads the interface's **source** (`import std;` → libstdc++'s `bits/std.cc`, MSVC's `modules/std.ixx`); a unit is identified by the `export module` it declares |
| Answers in | a file that compiles | a file being edited — half-typed lines included |
| Diagnostics | Clang's | its own 26 checks, measured at **zero false positives** on libstdc++'s own sources |

## What makes it different

**1. The program is the unit of work, not the file.** A C++ file does not mean what its characters say until a
preprocessor has run, so one runs: `#include`s are inlined, the `#if` branch actually in force is taken, macros are
expanded, and the parser reads the text a compiler would see. A typical `import std;` program is a **384-file,
4.85 MB** rendering parsed **once**, and every file that imports the module answers from that parse.

**2. Modules follow the standard, and the units are identified by what they *declare*.** The language defines the
semantics — a partition is internal to its module, `export import :part;` re-exports it, an implementation unit
(`module geometry;`) sees its module's names, and `std` is an implementation-provided module — and those are the rules
this server implements; what the language does **not** define is which *file* holds a unit (that mapping belongs to the
build) or the compiled module format. So `import std;` reads the implementation's own module source — libstdc++'s
`bits/std.cc`, MSVC's `modules/std.ixx`, each of which declares `export module std;` — and a project's units are
found by reading each candidate file: a file that does not declare `export module geometry:shapes;` is **rejected**, so
the naming conventions are a search order, not an assumption.

**3. No build system required.** Includes that no compiler ever looked at are found by a bounded fuzzy search under the
project root, so a Visual Studio solution and an unconfigured checkout both work. A compilation database, when present,
is used for exact per-file flags.

**4. Every claim in this repository is measured.** **2,000+ tests**, plus probes that run against real
headers rather than fixtures. [`docs/open-questions.md`](docs/open-questions.md) is a record of before/after numbers: a sweep of
libstdc++'s 2,500 headers, MSVC's 166 STL headers, and a construct-by-construct table of compiler extensions
(**GCC 40, Clang 29, MSVC 62**) all run in CI.

## Crates

| Crate | crates.io | What it is |
| --- | --- | --- |
| [`cpp_parser`](crates/cpp_parser) | [![crates.io](https://img.shields.io/crates/v/cpp_parser.svg)](https://crates.io/crates/cpp_parser) | A lossless C++ parser over preprocessed text: a rowan CST with error recovery, C++23/C23 features, CUDA, and vendor extensions |
| [`cpp_code_analysis`](crates/cpp_code_analysis) | [![crates.io](https://img.shields.io/crates/v/cpp_code_analysis.svg)](https://crates.io/crates/cpp_code_analysis)  | The preprocessor and cook, the index and its cache, symbol scopes, type inference, modules/partitions, diagnostics |
| [`cpp_ls`](crates/cpp_ls) | [![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls) | The server: LSP transport, handlers, configuration, and the binary |
| [`cpp_formatter`](crates/cpp_formatter) | [![crates.io](https://img.shields.io/crates/v/cpp_formatter.svg)](https://crates.io/crates/cpp_formatter) | An independent formatter with clang-format's configuration surface |

## What it answers

**Reading**

| Feature | Contents |
| --- | --- |
| Completion | Members after `.` / `->` / `::`, through pointers, smart pointers, bases and instantiated templates; names in scope; the Doxygen vocabulary and a symbol reference's target inside a documentation comment |
| Hover | The declaration, the deduced type, and the doc comment (Doxygen commands included) |
| Signature help | The parameter list being filled in, from the same facts the call is checked against |
| Definition / type definition / implementation | Where a name, its type, or the override of a virtual function is written — including a `` [`symbol`] `` a documentation comment refers to |
| References, rename, document highlight | Every use in the project, and a rename that edits exactly those |
| Call hierarchy, document and workspace symbols | Who calls what, and the shape of a file or a project |
| Folding, selection range | Blocks, comments and regions; the syntactic unit around the cursor |
| Format-string awareness | Completion inside `printf` / `std::format` / `fmt::format` strings, hover on a specifier, and four custom semantic-token types |

**Editing and checking**

- **Inlay hints** — the deduced type of `auto` and structured bindings, parameter names at a call.
- **CodeLens** — a function's reference count, on its declaration in a header and on its definition in a `.cpp`, counting the places that use it and not the declaration itself. One `find references` per function, **0.7–0.9 ms** warm and 9.7 ms for the first on a 1 086-file project (`tests/reference_cost_probe.rs` measures it).
- **Semantic tokens** — names, types, keywords, documentation commands and symbol references, and format strings
  (`formatSpecifier`, `formatArgument`, `formatConversion`, `formatEscape`).
- **Code actions** — quick fixes from the same diagnostics.
- **Diagnostics** — `textDocument/diagnostic` for pull-capable clients, otherwise versioned
  `textDocument/publishDiagnostics`, 26 codes: `syntax-error`, `include-not-found`,
  `undefined-name`, `undefined-field`, `redefinition`, `missing-return`, `argument-count`, `param-type-check`,
  `assign-type-check`, `return-type-mismatch`, `access-invisible`, `concept-not-satisfied`,
  `assignment-in-condition`, `unnecessary-branch`, `macro-redefined`, nine clang-tidy-shaped style rules
  (`cstyle-cast`, `use-auto`, `use-nullptr`, …), `unused`, `module-note` — each one enableable by name in
  `.cppls.toml`.

**C and CUDA.** C is a first-class dialect. CUDA is a **special toolchain case**: `.cu` and `.cuh` are read with
`<<<…>>>` launch blocks and the CUDA keywords, and a `[cuda]` section tells the analysis where the toolkit's headers
are. What a compiler enforces about device code is not modelled.

## Documentation comments

A documentation comment is **a language of its own**, not prose the analysis skips. `///` and `//!` mark one line;
`/**` and `/*!` open a block that stays documentation until its `*/`. Everything below works in all four, and in the
file's own text — a comment produces no tokens in the cooked reading, so every one of these questions is answered
from the buffer the reader is editing.

**Doxygen commands.** `@brief`, `@param`, `@return`, `@tparam`, `@exception`, `@see`, `@code`, `@name` and the rest of
the table are vocabulary the server knows:

- **hover** on a command explains it, and a declaration's own comment is rendered in its card;
- **completion** offers the command names (`/// @par` → `param`, `tparam`, `par`, `paragraph`); after a command that
  takes a name, the parameters of the declaration being documented (`@param |` → its parameters); and after one that
  takes a **reference** — `@see`, `@sa`, `@ref`, `@overload`, `@extends`, `@implements`, `@cite` — the program's
  names, exactly as a `` [`…`] `` link gets them;
- **semantic tokens** draw a command as `decorator`, which a client can remap on its own — the cppls extension draws
  `keyword.doxygen` from it;
- a command's *body* is the reader's, and the server does not colour it.

**Code blocks.** `@code` … `@endcode` is an example of code, not prose, and it is treated as one: the block's
keywords and resolvable names are coloured like the code around it, and completion inside the block offers what a
statement offers — keywords and snippets included. The body is read **per line with the comment marker removed**, so
`/// int main() { return 0; }` lexes as code rather than as another comment.

**Symbol references.** Rust's intra-doc link, in a C++ comment — and Doxygen's own spelling of the same thing:

```cpp
/// See [`Widget::grow`] for the rule, [`std::vector`] for the container, and [`grow()`] for the call.
/// @see Widget::grow
struct Widget { void grow(int by); };
```

| Position | What happens |
| --- | --- |
| The target | Coloured as the declaration it names — a function, a type, a macro — and **not** coloured when nothing declares it: a colour would say the name is known |
| hover | The declaration's card, the same one a name gets |
| definition | Jumps to the declaration; a macro's reference jumps to its `#define`, which is the only place a reader can go for one |
| completion | The names this file can see — and only names, never a snippet: a reference takes a symbol |

Both spellings are one question, answered in one place (`cpp_code_analysis::doc`), which is why the colour, the card
and the jump cannot disagree about what a reference names.

Four details worth stating, because they are decisions rather than accidents:

* **Only the backticked form.** `[target]` without the backticks is what Rust also accepts, and in a C++ comment it
  collides with the most ordinary prose there is: `arr[i]` and `matrix[row][col]`. The backtick is the whole
  disambiguator, and a target with whitespace in it is a title rather than a link.
* **`()` and `!` are punctuation, not part of the name.** `` [`grow()`] `` says "this is a call" and `` [`MAX_SIZE!`] ``
  says "this is a macro"; both are trimmed before the lookup, and the range still covers what was written.
* **A reference's body is prose.** `@see Foo for the rule` names `Foo`: a target never holds a space.
* **Prose offers nothing.** A cursor anywhere else in a documentation comment completes nothing at all — a comment is
  not a place a C++ name goes — while ordinary identifiers still get the client's own colour.

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

The server currently analyzes only the first workspace folder. If the client cannot dynamically register file
watches, a native watcher monitors that folder; watcher failures are reported to the client.

## Working on it

```bash
cargo test --workspace --release          # 2,000+ tests; the latency ones only measure in --release
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

[`docs/cpp-support.md`](docs/cpp-support.md) ·
[`docs/c-support.md`](docs/c-support.md) ·
[`docs/configuration.md`](docs/configuration.md) ·
[`docs/open-questions.md`](docs/open-questions.md) — what is measured and **not** fixed, with the evidence

## License

MIT. See [`LICENSE`](LICENSE).

Implemented by DeepSeek.
