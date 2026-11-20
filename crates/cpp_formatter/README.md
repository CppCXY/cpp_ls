# cpp_formatter

[![crates.io](https://img.shields.io/crates/v/cpp_formatter.svg)](https://crates.io/crates/cpp_formatter)
[![docs.rs](https://img.shields.io/docsrs/cpp_formatter.svg)](https://docs.rs/cpp_formatter)

**A C++ formatter whose configuration is clang-format's** — the same `.clang-format` keys, the seven presets, and the
same output shape where it can be measured. It is built on `cpp_parser`'s tree rather than on text, and it ships as
both a library and a binary:

```bash
cargo run -p cpp_formatter --bin cppfmt -- -style=Google file.cpp     # formatted text on stdout
cargo run -p cpp_formatter --bin cppfmt -- -i file.cpp                # in place
cargo run -p cpp_formatter --bin cppfmt -- -style=file -dump-config   # the resolved style
```

## What it decides, and what it does not

```text
decided        the spaces between tokens, the indentation of a line, where a `{` goes (BreakBeforeBraces and the
               BraceWrapping table), the order of an `#include` block, the alignment of a trailing comment, the
               escaped-newline layout of a macro body
not decided    **line breaking.** The line structure of the input is kept: a line that was broken stays broken, and
               `breaks`/`wrapping` are the modules the breaker is being built in
```

That last line is the honest limit, and it is why the language server does not run this formatter by default:
`[format] formatter` names the program `textDocument/formatting` runs, and its default is `clang-format` from `PATH`;
this one answers when a project asks for it (`formatter = "cpp_formatter"`). [`docs/formatter-diff.md`](../../docs/formatter-diff.md)
measures the gap against a named clang-format version and says what closing it costs.

## The safety argument

**The renderer copies every token out of the source and rewrites only a run of spaces and tabs** that contains no
comment and no newline. That asymmetry is the whole of it: a file this formatter ran over can never lose a token or a
comment, and a file it ran over and then ran through `clang-format` differs only where the *decisions* differ — which
is what the audit measures.

## The map

```text
config      `.clang-format` and the seven presets, as a flattened key/value style
presets     LLVM, Google, Chromium, Mozilla, WebKit, Microsoft, GNU
options     the style keys, their types, and the one place a key's spelling lives
tokens      the token stream with its trivia split into "gaps" — the only regions a formatter may rewrite
roles       what a token *is*: template angle, declarator, block brace — read from the syntax tree
space       the width between two tokens that stay on one line
render      the source with those widths substituted, and nothing else touched
layout      how deep each line sits: block lines, continuation lines, case labels, namespace bodies, directives
breaks      logical lines — the unit a line breaker works on (`UnwrappedLine`'s subset)
wrapping    where a `{` goes, and `else`/`catch`/`while` after `}`
functions   `AllowShortFunctionsOnASingleLine`: a one-statement body that fits becomes one line
includes    include sorting and regrouping (`IncludeCategories`, the main-header promotion)
comments    trailing comments, their alignment, and the column they may not pass
regions     `// clang-format off` / `on` and `DisableFormat`
columns     column bookkeeping for the indentation of a line that is being emitted
rows        the tab/space expansion a column count depends on
tables      the brace-wrapping and other per-construct tables
trailing    the pass over finished lines that aligns trailing comments
macros      escaped newlines in a macro body
output      `-output-replacements-xml`, as clang-format writes it
```

## Testing

```bash
cargo test -p cpp_formatter
```

```text
cases_*           one suite per construct family: blocks, classes, comments, functions, preprocessor, reflow,
                  short_functions, spacing, statements, tables, templates, types, wrapping
against_clang_format   the audit: the same input through both formatters, and the differences classified
layout_reflow     the indentation cases, pinned one by one
options           every style key parsed, defaulted and applied
```

The audit needs a `clang-format` binary and is `#[ignore]`d; the case suites are hermetic and are the gate.

## See also

- [`docs/formatter-diff.md`](../../docs/formatter-diff.md) — the measurement against clang-format, and the plan
- [`docs/configuration.md`](../../docs/configuration.md) — `[format] formatter`, and how the server chooses one

## License

MIT — see [LICENSE](../../LICENSE).
