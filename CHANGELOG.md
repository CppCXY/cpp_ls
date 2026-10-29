# Changelog

All notable changes to `cpp_ls` are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/),
and the project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] — 2026-10-29

The first release. A C and C++ language server written from scratch in Rust — parser, preprocessor, semantic
analysis and the protocol layer are all in this repository, with no clangd, no compiler daemon and no
`compile_commands.json` requirement.

### Language support

- **Preprocessor first**: macros are expanded, conditional compilation is resolved for the branches actually in force,
  and `#include`s are inlined; the resulting *rendering* is what gets parsed, which is why a file that has never
  compiled still resolves.
- **C and C++ (through C++23)**: `.h` is C++ and the language is read through its preprocessor. CUDA is a special toolchain case — `.cu`/`.cuh`, `<<<…>>>` launch blocks and the CUDA keywords, configured with a `[cuda]` section — rather than a first-class target:
  grammar.
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
