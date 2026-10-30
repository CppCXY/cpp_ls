# cpp_ls

A **C and C++ language server written in Rust**, implemented by Deepseek: the parser, the preprocessor, the semantic
analysis and the LSP layer are all in this repository. No clangd, no compiler daemon, and no `compile_commands.json`
required.

One design decision shapes everything else: a C++ file does not mean what its characters say until a preprocessor has
run. The server expands macros, resolves conditional compilation for the branches actually in force, inlines
`#include`s, and parses the text that comes out. That is what lets completion, hover, diagnostics and the rest answer
in a file that has never been compiled — including a half-typed line.

## Scope

**C and C++ are the target.** CUDA is supported as a **special toolchain case**, not as a first-class target: `.cu` and
`.cuh` files are read with `<<<…>>>` launch blocks and the CUDA keywords, and a `[cuda]` section tells the analysis
where the toolkit's headers are and which host compiler decides the dialect. What a compiler enforces about device code
is not modelled.

## What it answers

**Reading code**

| Feature | What it answers |
| --- | --- |
| Completion | Members after `.` / `->` / `::`, through pointers, smart pointers, bases and instantiated templates; names in scope; keywords; `#include` paths; macro names and their parameters |
| Hover | The declaration, the type — including the deduced type of `auto` and `decltype` — and the documentation comment, Doxygen commands included |
| Signature help | The parameter list being filled in, from the same facts the call is checked against |
| Definition, type definition, implementation | Where a name, its type, or the override of a virtual function is written |
| References, rename, document highlight | Every use of a name in the project, and a rename that edits exactly those |
| Call hierarchy, document and workspace symbols | Who calls what, and the shape of a file or a project |
| Folding, selection range | Blocks, comments and regions; the syntactic unit around the cursor |

**Editing**

- **Inlay hints** — the deduced type of `auto` and structured bindings, and parameter names at a call.
- **Formatting** — see the section below; it is the one feature with a caveat.
- **Semantic tokens** — names, types, keywords and documentation commands, plus four custom token types (`formatSpecifier`,
  `formatArgument`, `formatConversion`, `formatEscape`) that the VS Code client colours the way rust-analyzer colours a
  format string.
- **Code actions** — quick fixes that come from the same diagnostics the next section describes.

**Diagnostics**

Pull-only (`textDocument/diagnostic`), 26 codes across errors, style rules and notes — `syntax-error`,
`include-not-found`, `undefined-name`, `undefined-field`, `redefinition`, `missing-return`, `argument-count`,
`param-type-check`, `assign-type-check`, `return-type-mismatch`, `access-invisible`, `concept-not-satisfied`,
`assignment-in-condition`, `unnecessary-branch`, `macro-redefined`, the nine clang-tidy-shaped style codes
(`cstyle-cast`, `use-auto`, `use-nullptr`, …), `unused` and `module-note`. Every code can be enabled or disabled by
name in `.cppls.toml`.

The layer is built against one measurement: **zero false positives over the standard library's own sources** — a
project of real, correct code is where a checker earns the right to report anything on a project of someone else's.

**Format strings** are understood, not just highlighted: completion inside `printf`/`std::format`/`fmt::format` strings,
hover on a specifier, and the colouring above. `[format]` names the project's own wrappers.

## Formatting is not clang-format

The built-in formatter (`cpp_formatter`) is an **independent implementation that imitates clang-format**, not a port of
it and not the same program. It reads the nearest `.clang-format` — `BasedOnStyle` and the seven presets, plus the
options this implementation honours — so a project that already has one gets sensible output out of the box, and it
answers `textDocument/formatting`, `rangeFormatting` and `willSaveWaitUntil`.

It exists so that the server is usable with nothing else installed. Its output is idiomatic, not byte-identical to
clang-format's, and it does not implement every option.

**For production code, use clang-format.** Point the editor's formatter at it instead of this one — in VS Code, a
`[cpp]`/`[c]` `editor.defaultFormatter` such as `xaver.clang-format`, or format on save through clang-format — and treat
the built-in formatter as the fallback that makes a bare install work.

## Building

```bash
cargo build --release -p cpp_ls          # one binary, LSP over stdio by default
target/release/cpp_ls --help            # tcp, log level, config path, standard library switches
```

The VS Code client is [CppCXY/VSCode-Cppls](https://github.com/CppCXY/VSCode-Cppls); any other LSP client can be
pointed at the binary directly.

## Configuring a project

A `.cppls.toml` in the workspace root, or a `compile_commands.json` for the exact compiler flags — both are described in
[`docs/configuration.md`](docs/configuration.md). No configuration is needed for a project of plain sources.

## Working on it

```bash
cargo test --workspace --all-features    # tests; the latency ones only measure in --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

## License

MIT. See [`LICENSE`](LICENSE).
