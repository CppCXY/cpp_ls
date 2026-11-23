# cpp_toolchain

[![crates.io](https://img.shields.io/crates/v/cpp_toolchain.svg)](https://crates.io/crates/cpp_toolchain)
[![docs.rs](https://img.shields.io/docsrs/cpp_toolchain.svg)](https://docs.rs/cpp_toolchain)

**The toolchain and the preprocessor** — how a translation unit is found, configured, walked and rendered. It is the
layer under `cpp_code_analysis`, and it answers what a *compiler's inputs* say, never what a name means.

```text
project      `.cppls.toml`, `compile_commands.json`, `build_database.json`, the discovered toolchain
include      where `#include "x.h"` and `#include <x.h>` actually look, and which compiler's headers those are
walk         the translation unit: which file includes which, and which macros are in force where
cook         one file's text as the compiler sees it: the rendering, and the spans back to the file
preprocess   directives, macros, conditions, guards, expansion
paths        the files a reader's questions are answered from
```

## The rule this crate enforces by existing

The dependency is one-way: `cpp_code_analysis` depends on this crate, and this crate may not depend on it. What that
forbids is the thing the split is for — **the preprocessor may not ask the index what a name means.** A question that
needs the index belongs above; this layer answers from the compiler's own inputs and says `Unknown` when they do not
say it.

## The unit of work: one file's rendering

```text
text ──lex──▶ tokens ──apply the environment──▶ tokens ──expand──▶ the rendering
                                                    │                  │
                                         the macro environment   spans back to the file
                                         (from the walk)         (what a hover maps through)
```

A *rendering* is the file with its directives applied, its macros expanded and its comments gone, plus the span table
that maps every byte of it back to the file the reader wrote. That unit is what the analysis's cache keys and stores,
which is why it lives here: [`preprocess::cooked`](src/preprocess/cooked.rs) is the one place the two layers meet,
through two small traits (`MacroEnvironment`, `UnitDefinitionsView`) rather than through a dependency.

## The project's build description

Both database formats are read into one structure — one command per translation unit, with its working directory:

```text
compile_commands.json   every build system exports it (CMake, Ninja, Meson, Bazel, Bear, …)
build_database.json     CMake 3.31's module-aware one (`CMAKE_EXPORT_BUILD_DATABASE`, Ninja generators)
```

Discovery is the root and three directories below it, `compile_commands.json` wins when a directory has both, response
files (`@CMakeFiles/<target>.dir/includes_CXX.rsp`) are expanded, and a project can name one explicitly with
`[compile] database` in `.cppls.toml`. The formats, what is read from each, and what is deliberately not (module
metadata, for now) are in [`docs/configuration.md`](../../docs/configuration.md#the-two-database-formats).

## The map

```text
project      `.cppls.toml`, the databases, discovery, and the toolchain the flags came from
include      the compiler's include search: user and system paths, the standard library, one spelling's resolution
preprocess   the walk (one translation unit's files, order and macro timelines) and the renderer that expands one file
paths        file providers: `DiskFiles`, and the in-memory one a test opens a buffer in
token        the token and trivia vocabulary the renderer works in
hash         the hashing a cache key is made of
probes       the tracing switches this crate answers to
```

## Testing

```bash
cargo test -p cpp_toolchain
```

```text
expansion_budget                    how many expansions one file may cost, and what the bound is for
expansion_spans_probe              the span table: every expansion, mapped back
inactive_regions_probe             which regions a configuration does not compile — what an editor greys out
preprocessor_against_the_compiler  the parser's view of a real header against a real compiler's
```

The last one needs a toolchain on the machine; it is `#[ignore]`d for that reason and run by hand and in the probes
job. Everything else is hermetic.

## See also

- [`docs/configuration.md`](../../docs/configuration.md) — the build description, `.cppls.toml`, and the limits
- [`crates/cpp_ls/src/handlers/inactive_regions`](../cpp_ls/src/handlers/inactive_regions) — what "this region does not compile" means here
- [`crates/cpp_code_analysis`](../cpp_code_analysis) — the layer above, which asks this one for readings

## License

MIT — see [LICENSE](../../LICENSE).
