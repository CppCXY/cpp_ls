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
- **An elaborated `struct bits` no longer declares a type of its own.** `struct bits make_bits(...)`, `struct bits
  value;` and a `struct bits` parameter use the tag as a type, but the scope builder bound the bodyless class as a
  declaration in that file: a `bits` type with no members shadowed the header's definition, so `members_of("bits")`
  answered 0 and `pointer->three` reported no member while the header's summary held all four. The same held for
  `enum Color pick(void);`, where a jump on `Color` landed on the function. A tag is now bound only when its
  declaration introduces nothing else (`struct bits;`, `class Widget;`). `FORMAT_VERSION` 59 → 60.
- **Parser: the compiler-extension spellings missing from the vendor grammar.** `__alignof__` and
  `__typeof_unqual__` lex as their keywords; `_Atomic` (C11) is a type specifier in both spellings and a qualifier
  after `*`; Clang's `_Nullable`/`_Nonnull`/`_Null_unspecified` are consumed instead of becoming a parameter's name;
  `_Decimal32/64/128`, `_Float32x/64x` and `__float80` are types for a GNU target; and MSVC's `__asm` is a statement
  in both its braced and its traditional brace-less form. GNU nested functions are read in a GNU C configuration,
  `__auto_type` is the `auto` keyword, and `__based`/`__sptr`/`__uptr` are read where they are written.
  `_Complex`/`__complex__` are type spellings, so a file-scope `_Complex double z;` is one declaration whose recorded
  type is `_Complex double`, not a macro followed by a separate `double z;`.
- **GNU's `__label__` declares labels, not variables.** `__label__ done, again;` binds each name in the label
  namespace, so the facts are labels (`Other`, no type) — the same kind a `done:` line produces — instead of variables
  whose type is `__label__`.
- **A diagnostic is never drawn on a branch the cook did not compile.** Checks read the file's own summary, which
  keeps every `#if` branch; `include-not-found` already asked the guard layer, but that layer answers `Unknown` for a
  name the macro walk does not know. Every finding now passes one more filter in `CheckContext::push`: the cook's
  inactive regions, in the file's own coordinates. Measured on neovim's `os/fs.c`, the four dead-branch includes
  (`<sys/acl.h>`, `<sys/access.h>`, `<sys/xattr.h>`, `<sys/uio.h>`) and one dead `unused` variable went silent, while
  live findings in `buffer.c`, `window.c`, `tui.c` and `api/vim.c` were unchanged.
- **A struct of `_BitInt` bitfields has a layout.** `_BitInt(N)` is sized from the measured rule (1/2/4/8 bytes up to
  64 bits, then multiples of eight, alignment 8 above 8 bytes), and bitfields are placed by the Itanium/GCC x64
  storage-unit rules instead of withholding the whole class: the `:` and its literal width are read from the member's
  own file text, the card prints `(bits a..b)`, and `struct bits { unsigned _BitInt(3) three : 3; … }` now reports
  `size 6 byte(s), alignment 2` with offsets 0/2/4/5 — measured against gcc. A width that is a macro, an MSVC
  bitfield (whose ABI differs), and unnamed bitfields still withhold the layout rather than guess.
- **The `__has_*` operators are asked of the compiler.** `__has_builtin`, `__has_feature`, `__has_extension`,
  `__has_attribute` and `__has_declspec_attribute` were `Unknown`, so their branches took the else arm. A
  `CompilerFeatureProbe` renders the question as a tiny translation unit and runs the project's compiler over it
  (GNU/Clang spelling; MSVC stays `Unknown`), caching by operator, operand, language and standard; measured on a
  MinGW project, `#if __has_builtin(__builtin_expect)` now renders the taken branch. `__has_include` still belongs to
  the include search, and a compiler that does not have the operator still answers `Unknown` rather than `0`.
  `FORMAT_VERSION` 60 → 61 (both changes alter which declarations a stored reading holds).

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
