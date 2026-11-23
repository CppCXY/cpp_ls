# cpp_ls

[![crates.io](https://img.shields.io/crates/v/cpp_ls.svg)](https://crates.io/crates/cpp_ls)
[![docs.rs](https://img.shields.io/docsrs/cpp_ls.svg)](https://docs.rs/cpp_ls)

**A C, C++ and CUDA language server.** The binary is the whole server: LSP transport, one handler per request, the
configuration, and the engine behind it (`cpp_toolchain` → `cpp_code_analysis`).

```bash
cargo run -p cpp_ls                                    # LSP over stdio
cargo run -p cpp_ls -- --communication tcp --port 5007 # a server a client connects to (development)
cargo run -p cpp_ls -- --help                          # every switch, with its default
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--communication` | `stdio` | `stdio` or `tcp` |
| `--ip`, `--port` | `127.0.0.1`, `5007` | where a TCP server listens |
| `--log-level` | `info` | `error`, `warn`, `info`, `debug`, `trace` |
| `--log-path` | `none` | a log file, or `none` |
| `--config` | `none` | a `.cppls.toml` somewhere other than the workspace root |
| `--editor` | unset | `vscode`, `intellij`, `neovim` — capabilities that differ by client |

## What it answers

One module per request under [`src/handlers`](src/handlers):

```text
completion  hover  signature_help  definition  type_definition  implementation  references  rename
document_symbol  workspace_symbol  call_hierarchy  type_hierarchy  document_highlight  selection_range
folding_range  document_link  code_action  inlay_hint  semantic_token  formatting  format
diagnostic  inactive_regions  text_document  configuration  initialized  switch_source_header
```

Two of those are not LSP:
[`textDocument/switchSourceHeader`](src/handlers/switch_source_header) (clangd's extension method, and the reason the
VS Code client ships a `cppls.switchheadersource` command) and the **inactive regions** notification — the ranges a
configuration does not compile, which an editor greys out. The switch-source-header method and the configuration it
reads are described in [`docs/configuration.md`](../../docs/configuration.md); the notification's own contract is in
[`src/handlers/inactive_regions`](src/handlers/inactive_regions).

## Design

- **The engine is not the protocol.** Everything below `handlers` is `cpp_code_analysis`; a handler asks a question
  and formats the answer. Nothing in this crate decides what a program means.
- **Nothing is answered before it can be.** A file's includes have to be read before a name in it can be resolved, so
  the server pumps the index in slices and answers from what it has; a client sees progress notifications rather than
  a hung request.
- **The server never guesses to fill a card.** `Known::Unknown(reason)` reaches the client as the reason, not as a
  declaration that happens to share a name.

## Testing

```bash
cargo test --release -p cpp_ls --tests     # the suite; `--release`, because the latency ones only measure there
```

```text
protocol       handshake lsp_status progress diagnostics_stable watch_rescan
answers        hover_attributes hover_layout implementation_declaration module_implementation
               reported_hover a_header_language formatter_choice
latency        latency idle
real projects  real_project
allocator      global_allocator — the binary installs `MiMalloc` itself, and no shipped `src/` wraps `GlobalAlloc`
```

The suite is **one test target** (`tests/suite.rs`) over the modules in `tests/suite/`. The real-project ones are
`#[ignore]`d and read the project from `CPPLS_PROJECT`, `CPPLS_C_PROJECT` or `CPPLS_LUA_PROJECT`.
`cargo test --release -- --ignored --nocapture` runs the set, and each prints what it measured.

[`cpp_inspect`](../cpp_inspect) asks the same engine one question without an editor — the instrument every report in
[`docs/open-questions.md`](../../docs/open-questions.md) was measured with:

```bash
cargo run --release -p cpp_inspect -- --project <root> --file <file> hover --needle "node->twin" --text
cargo run --release -p cpp_inspect -- --fixture "a.cpp=struct Widget { int size() const { return 1; } };" --needle size hover
```

Its protocol and every command are in [`docs/inspect.md`](../../docs/inspect.md); `caps` prints the list.

## See also

- [`docs/configuration.md`](../../docs/configuration.md) — `.cppls.toml`, the two database formats, and the limits
- [`crates/cpp_formatter`](../cpp_formatter) — the built-in formatter `textDocument/formatting` can use

## License

MIT — see [LICENSE](../../LICENSE).
