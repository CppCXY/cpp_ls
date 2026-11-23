# Changelog

All notable changes to `cpp_ls` are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/),
and the project adheres to [Semantic Versioning](https://semver.org/).

## [0.5.0]

fix many parser and toolchain bug

### Removed

- **The global allocator's byte counter.** `cpp_ls` installed a `Counting` wrapper around `MiMalloc` so
  `CPPLS_TRACE_ALLOCS` could report a stage's *held* bytes; every allocation and free in every release paid a relaxed
  atomic load for a trace that was off by default. The binary now installs `MiMalloc` directly, and the counter, its
  module (`cpp_code_analysis::alloc`), the `CPPLS_TRACE_ALLOCS` and `CPPLS_LIVE_CEILING_MB` variables and every
  `[memory]` line it fed are gone. What remains is `CPPLS_TRACE_MEMORY`, the session's own table estimate
  (`Session::memory_report`) — a measurement of what the analysis holds, not of the process. The pump and query traces
  the counter had been gating are now `CPPLS_TRACE_PUMP` and `CPPLS_TRACE_QUERY`.
- **The dead `perf-probe` feature.** Its only driver was `tests/allocation_probe.rs`, deleted with the other probes in
  `2dc17c8d`; every mark recorded a counter nothing incremented. The feature, `cache/perf.rs`, its marks and the
  `mimalloc-safe` dev-dependency are gone.

### Fixed

- **`__FILE__` answers the compile command's spelling, and `#line N "name"` is applied.** `__FILE__` used to be the
  empty literal in every rendering and `#line` was parsed and then dropped: a file whose command writes `src/a.cpp`
  now expands it to `"src/a.cpp"`, and a `#line` changes the line and file the following lines report. A GNU line
  marker (`# 100 "x.c"`), a `#line` whose number or name is a macro, and a `#line` in force at a hover card's use are
  read as well, and a condition may ask the two names (`#if __LINE__`, `#elif`, `#ifdef`, `defined`); `__LINE__`
  written directly in a run also answers its own line now, where it used to answer the run's first line.
  `context_hash` folds the spelling, so `FORMAT_VERSION` moved 57 → 58.

## [0.4.0]

### Added

- **`build_database.json`** — CMake 3.31's module-aware build database (`CMAKE_EXPORT_BUILD_DATABASE`, Ninja generators)
  is read as a compilation database, next to `compile_commands.json`: same per-file arguments, plus `work-directory`
  resolution and the `baseline-arguments` + `local-arguments` split. A directory that has both is read as
  `compile_commands.json`, which describes the build that ran. See
  [`docs/configuration.md` §1](docs/configuration.md#the-two-database-formats).

### Fixed

- **A type is spelled from its declarator, not from the shape written back.** `int (*table[3])(int);` was recorded as
  `int ([3]*)(int)`; a `typedef` of a function pointer kept its parameter *names* (`int (*)(int left, int right)`), so a
  consumer comparing spellings read a different type. Both now come from the declarator's own text —
  `int (*[3])(int)` and `int (*)(int, int)`.
- **A function returning a function pointer records the pointer it returns**: `int (*maker(int a))(int b);` answers
  `returns int (*)(int b)` rather than `int`.
- **`void (lua_setfield) (…)` and the other parenthesised declarators** — a declarator keeps its own suffixes now, so a
  function returning a function pointer is a *function* (`int (*maker(int a))(int b);` was a variable of type
  `int(int)*`), and a typedef of a pointer (`typedef struct StackValue *StkId;`) is followed for a member access
  (`s->val`).


## [0.1.0] — 2026-10-29

The first release. A C and C++ language server written from scratch in Rust — parser, preprocessor, semantic
analysis and the protocol layer are all in this repository, with no clangd, no compiler daemon and no
`compile_commands.json` requirement.

### Language support

- **Preprocessor first**: macros are expanded, conditional compilation is resolved for the branches actually in force,
  and `#include`s are inlined; the resulting *rendering* is what gets parsed, which is why a file that has never
  compiled still resolves.
- **C, C++ and CUDA**: `.h` is C++ and the language is read through its preprocessor; the grammar covers C++11 through
  C++26, including static reflection and contracts, which are read without a feature bit
  ([`docs/cpp-support.md`](docs/cpp-support.md)). CUDA is a special toolchain case — `.cu`/`.cuh`, `<<<…>>>` launch
  blocks and the CUDA keywords, configured with a `[cuda]` section — rather than a first-class target.
- **Coroutines**: `co_await`, `co_yield` and `co_return` are parsed, typed (`co_await` answers what `await_resume`
  returns), hovered and exempted from the missing-return rule.

### Editor features

```
completion · hover · signature help · go to definition / type definition / implementation · references
document highlight · rename · call hierarchy · document and workspace symbols · folding · selection range
formatting · inlay hints · semantic tokens · code actions · diagnostics · #include links
```

- **Semantic tokens** cover names, types, keywords, doc commands, and — for VS Code, through the four contributed
  token types — format strings: specifiers, arguments, conversions and escapes.
- **Format strings** are understood for `printf`/`std::format`-style calls: completion inside a format string,
  hover on a specifier, and the colouring above.
- **Formatting** is clang-format-compatible in configuration (`.clang-format` and the seven presets) and works on a
  document or a range.
- **Inlay hints**: deduced types for `auto` and structured bindings, parameter names at a call.
- **Diagnostics**, pull-based (`textDocument/diagnostic`), 26 codes:

  ```
  errors    syntax-error  include-not-found  macro-redefined  undefined-field  undefined-name  redefinition
            argument-count  param-type-check  assign-type-check  return-type-mismatch  missing-return
            access-invisible  concept-not-satisfied  assignment-in-condition  unnecessary-branch
  style     cstyle-cast  use-auto  use-nullptr  use-override  braces-around-statements  else-after-return
            simplify-boolean-expr  macro-parentheses  implicit-bool-conversion
  notes     unused  module-note
  ```

### Configuration

- `.cppls.toml` for the project's own settings, and `compile_commands.json` for the exact compiler flags; both are
  documented in [`docs/configuration.md`](docs/configuration.md). None is required for a project of plain sources.
- Diagnostics can be enabled and disabled by code.

[0.1.0]: https://github.com/CppCXY/cpp_ls/releases/tag/v0.1.0
