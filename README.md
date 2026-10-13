# cpp_ls

**Work in progress — under development.** There is no release, no stability promise, and the
internals move. Everything below is what runs today, not what is planned.

A C++ language server written in Rust. It reads a project the way a compiler does: macros are
expanded, conditional compilation is resolved for the branches actually in force, `#include`s are
inlined, and the resulting *rendering* is what gets parsed — because a C++ file does not mean what
its characters say until a preprocessor has run.

## What works today

```
cargo build --release --workspace --all-targets     no errors, no warnings
cargo test  --release --workspace                   green
```

- **Parsing and rendering** of a real project: zero syntax errors over the whole MSVC standard-library
  closure (187 files).
- **Thirteen semantic checks**, each with a `DiagnosticCode` (carried to the client as the LSP
  `code`), a severity and, where it applies, a tag:

  ```
  syntax-error  include-not-found  macro-redefined  unused  access-invisible
  missing-return  assignment-in-condition  unnecessary-branch  undefined-field
  argument-count  param-type-check  assign-type-check  return-type-mismatch
  ```

- **Zero false positives on the standard library** — the measurement this layer is built to pass:

  ```
  cargo run --release -p cpp_code_analysis --example checks_probe -- target/std-cook target/std-cook/assign-case.cpp
    assign-case.cpp:52  [assign-type-check]  `x` is initialised with a value that does not convert to `std::string`
    --- checks over 5 file(s) ---   1  assign-type-check
  ```

  Exactly one true positive, nothing else. Fixtures used by that measurement live in `target/std-cook/`.

- **A finding is about the file being edited; the knowledge behind it is the project's.** A check
  reports only on what this file writes, but it may read the index's facts about any other file — so
  `std::string x = 123;` is judged from the parameter list of `std::string`'s constructors recorded in
  the index, not by parsing `<string>` again.

- **The server side**: diagnostics, completion, signature help, go to definition, references,
  semantic tokens, folding, inlay hints.

## Where it is going

**Configuring a project** — `compile_commands.json` and `.cppls.toml`, including the multi-target form that a
real C++ project needs: [`docs/configuration.md`](docs/configuration.md).
See [`docs/status.md`](docs/status.md) — one document, describing the current state, the defects that
are known and measured, and the plan. (It replaces nineteen older documents that described the
process rather than the state of the code, and had drifted out of date.)

## Trying it

```bash
cargo run --release --workspace --bin cpp_ls        # the server, speaking LSP on stdio

# the checks over a real project, one command
cargo run --release -p cpp_code_analysis --example checks_probe -- <include-dir> <root file>

# one check, with the declaration facts each finding came from
cargo run --release -p cpp_code_analysis --example one_check -- <include-dir> <root file> unused
```

## Layout

```
crates/cpp_parser           lexer and grammar; a lossless tree plus a typed AST over it
crates/cpp_code_analysis    preprocessing and rendering, the index, the semantic model, the checks
crates/cpp_ls               the language server: LSP handlers and the session they run against
```

