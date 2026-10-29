# cpp_ls

A **C, C++ and CUDA language server written in Rust**, implemented by Deepseek: parser,
preprocessor, semantic analysis and the LSP layer are all in this repository, with no clangd, no compiler daemon and
no `compile_commands.json` required.

It reads a project the way a compiler does — macros expanded, conditional compilation resolved for the branches in
force, `#include`s inlined — and the text the preprocessor produces is what gets parsed. That is what lets completion,
hover, diagnostics and the rest work in a file that has never been compiled, including a half-typed line.

```
completion · hover · signature help · go to definition / type definition / implementation · references
document highlight · rename · call hierarchy · document and workspace symbols · folding · selection range
formatting · inlay hints · semantic tokens · code actions · diagnostics · #include links
```

## Building

```bash
cargo build --release -p cpp_ls          # one binary, LSP over stdio by default
target/release/cpp_ls --help            # tcp, log level, config path, std library switches
```

The VS Code client is [CppCXY/VSCode-Cppls](https://github.com/CppCXY/VSCode-Cppls); any other LSP client can be
pointed at the binary directly.

## Configuring a project

A `.cppls.toml` in the workspace root, or a `compile_commands.json` for the exact compiler flags — both are described
in [`docs/configuration.md`](docs/configuration.md). No configuration is needed for a project of plain sources.

## Working on it

```bash
cargo test --workspace --all-features    # tests; the latency ones only measure in --release
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

## License

MIT. See [`LICENSE`](LICENSE).
