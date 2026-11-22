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
configuration does not compile, which an editor greys out. Both are described in
[`docs/configuration.md`](../../docs/configuration.md) and [`docs/inactive-regions.md`](../../docs/inactive-regions.md).

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
protocol       handshake lsp_status diagnostics_status diagnostics_stable progress format_status
               module_implementation implementation_declaration
answers        hover_probe lsp_hover_probe real_project_hover_probe rendering_hover_probe
               hover_positions_probe hover_attributes hover_layout reported_hover
               module_definition_probe rendering_members_probe signature_real implementation_real definition
latency        latency hover_latency idle idle_probe loop_probe memory_probe
real projects  real_project c_project arrow_status neovim_usability usability_probe codestyle_round_probe
               lua_diagnostics_probe macro_member_probe implementation_c_probe std_string_probe
               editor_answers_probe code_lens_probe
```

The real-project ones are `#[ignore]`d and read the project from `CPPLS_PROJECT`, `CPPLS_C_PROJECT` or
`CPPLS_LUA_PROJECT`. `cargo test --release -- --ignored --nocapture` runs the set, and each prints what it measured.

`tools/inspect` asks the same engine one question without an editor — the instrument every report in
[`docs/open-questions.md`](../../docs/open-questions.md) was measured with:

```bash
cargo run --release -p inspect -- --project . --file src/lib.rs hover --line 10 --col 5
cargo run --release -p inspect -- --fixture "a.c=int f(void) { return 1; }" --file a.c --cook --index type --line 1 --col 21
```

## See also

- [`docs/configuration.md`](../../docs/configuration.md) — `.cppls.toml`, the two database formats, and the limits
- [`docs/usability.md`](../../docs/usability.md), [`docs/neovim.md`](../../docs/neovim.md) — what a client has to do
- [`crates/cpp_formatter`](../cpp_formatter) — the built-in formatter `textDocument/formatting` can use

## License

MIT — see [LICENSE](../../LICENSE).
