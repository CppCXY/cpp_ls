# cpp_check

**The analysis's checks, run over a whole workspace** — CI's half of the same engine an editor reads. Not published
(`publish = false`): it is a front end for this repository's own rules.

```bash
cargo run --release -p cpp_check                        # the current directory, findings at `warning` and above
cargo run --release -p cpp_check -- E:\neovim           # a project
cargo run --release -p cpp_check -- --min-severity=hint # style hints too
cargo run --release -p cpp_check -- --json              # machine-readable
cargo run --release -p cpp_check -- --code=use-after-free,dangling-return
cargo run --release -p cpp_check -- E:\neovim --file src/nvim/cursor.c --file src/nvim/api/vim.c
cargo run --release -p cpp_check -- E:\neovim --files-from changed.txt
```

## What it is, and what it is not

It is **the same run a reader gets in an editor**: one `cpp_code_analysis::Session` over the project — the compile
database, the toolchain's include search, `.cppls.toml` — each file opened and read the way a compiler reads it, and
`Session::diagnostics`. So every finding here is a finding the editor would show on the same line, and a rule fixed in
the server is fixed here too. Nothing in this crate decides what is wrong with a program.

It is **not a compiler**. The compiler is the authority on whether a program builds. Run this for the rules a
compiler does not have — a pure virtual nothing overrides anywhere in the workspace, a name nothing uses, an include
that is missing, a reference to something that dies — and for the files a build never sees.
[`docs/cpp-check.md`](../../docs/cpp-check.md) has the split, measured.

## The reading step is the cost, and it is per file

A check that resolves names against a file whose macros are unexpanded is a check about a program nobody compiles, so
every file is **cooked** before it is asked about: `did_open` puts the text in the VFS, `catch_up` reads its summary,
`cook` renders the closure and files the cooked reading `diagnostics` answers from — the server's own three steps, in
the same order.

That is why **a named file set is the practical mode**: C++ locality is strong, and every file pays for its own
include closure. `--file` and `--files-from` skip the walk; `ROOT` still supplies the database and flags, so the files
are read the way the build reads them.

## Exit status

A check run is a gate: `--min-severity` says which findings are fatal, and a run with any of them exits `1`. The
default (`warning`) deliberately excludes hints — a hint is a suggestion, and a build that fails on a suggestion is a
build somebody turns the gate off for.

## The map

```text
cli     the arguments, and what they mean
walk    which files are the project's, and in what order
run     the session's steps: open, catch up, cook, ask, in slices under a budget
rules   the rules a run may report, with their codes and severities
report  the text and JSON output
```

## Testing

```bash
cargo test -p cpp_check
```

The tests sit beside the code they are about (`src/cli.rs`, `src/run.rs`, `src/walk.rs`, `src/rules.rs`,
`src/report.rs`, and `src/tests.rs` for the arithmetic they share): a position as a line and column, the walk's file
set and what it skips, the code filter's resolution against the rules, and the report's two shapes.

## See also

- [`docs/cpp-check.md`](../../docs/cpp-check.md) — the split from a compiler, measured on a real project
- [`crates/cpp_code_analysis`](../cpp_code_analysis) — the engine, where every rule lives

## License

MIT — see [LICENSE](../../LICENSE).
