# cpp_code_analysis

[![crates.io](https://img.shields.io/crates/v/cpp_code_analysis.svg)](https://crates.io/crates/cpp_code_analysis)
[![docs.rs](https://img.shields.io/docsrs/cpp_code_analysis.svg)](https://docs.rs/cpp_code_analysis)

**What the code means.** `cpp_parser` answers what a file *looks like*; `cpp_toolchain` answers what the compiler
*reads*; this crate answers the questions a reader asks about it — scopes, declarations, types, members, overloads,
modules, and the diagnostics an editor shows.

```text
a rendering, mapped back   ->  the file's declarations              (index, summary)
a name at an offset        ->  a declaration, or why not            (sema::resolve, index::project)
a call                     ->  the overload, and what it returns    (index::project::calls)
a cursor                   ->  members, parameters, a card          (completion, inlay, hover helpers)
a project                  ->  an index, its cache, and a session   (index::store, cache, session)
```

Entry point for a client is [`Session`]: it holds the files, the project configuration, the index and its cache, and
answers one question at a time without ever blocking the caller on a cook it has not asked for.

## Why the preprocessor is a crate below, and not a pass inside this one

The obvious design — preprocess, then parse the output — is what a compiler does, and it is unusable for an editor:

- **the tree has to stay lossless**, so expansion produces a *shadow* token stream (the rendering) and never rewrites
  the tree;
- **both branches of an `#if` have to stay visible**, because the reader may be writing the other one;
- **every edit would invalidate everything**, since one `#define` changes every file that includes it.

So the rendering is a unit produced below, keyed by its text and its environment, and this crate reads it.

## The map

```text
index        the project as one thing: symbols across files, members, definitions, calls, modules, and the cache key
sema         one file's scopes and meanings: resolve, types, declarations, checks, layout, call syntax, deduction
session      the front door: files, configuration, the pump, question-shaped queries, the database
cache        summaries on disk: the key, the store, the reader and the format version
file         one file's view: `FileView`, the coordinate conversions, and the documentation node of a comment
```

Three contracts worth knowing before changing anything here:

1. **A stored summary is a promise.** `cache::FORMAT_VERSION` moves whenever what a fact *says* changes — a kind, a
   spelling, a parameter list — because a summary written by another reader is a wrong answer and not a missing one.
   Each move carries its own measurement in the constant's documentation.
2. **A question about a position is asked of the rendering; a question about a macro is asked of the file.** The two
   views are different (`view` and `view_of_the_file`) and are not interchangeable; a macro's name does not exist in
   the rendering, and a declaration inside a `#if` branch does not exist in the file.
3. **Nothing may guess to avoid saying "not known".** `Known::{Yes, No, Unknown(reason)}` is deliberate: a caller
   that gets `Unknown` is told *which* spelling stopped the walk.

## Two readings of one expression

A macro invocation has no type in the file's own text and a full type in the rendering, so a type question whose
answer is `Unknown` may be asked again of the rendering — by span, and with the rules recorded in
[`docs/open-questions.md`](../../docs/open-questions.md) (the `MACRO(x)->member` round). That crossing is why
`ci_func(ci)->p` answers `Proto*` on Lua 5.5.0.

## Testing

```bash
cargo test -p cpp_code_analysis                    # the suites below
CPPLS_TRACE_TYPES=1 cargo test -p cpp_code_analysis --test macro_member_types -- --nocapture
```

```text
real projects   c_corpus cpp_corpus neovim_probe codestyle_probe real_project_probe lua_source_reports reported_defects
language        types scopes members modules concepts contracts reflection generics layout lifetime checks
reading         cook_fixture_probe cooked rendered_reading rendering_real rendering_scope unit_*_probe expand
cost            allocation_probe memory_probe cold_start_memory_probe namespace_macros slice_probe scale_probe
```

Real-project tests read a project from the environment and are `#[ignore]`d (`CPPLS_C_PROJECT`, `CPPLS_PROJECT`,
`CPPLS_LUA_PROJECT`, …); the hermetic ones run in CI. `docs/open-questions.md` is where each of them comes from, with
the before/after numbers.

## Probes

Every claim in this repository is measured, and the instruments are environment switches documented with what they
print in [`crates/cpp_toolchain/src/probes.rs`](../cpp_toolchain/src/probes.rs) — `CPPLS_TRACE_TYPES`,
`CPPLS_TRACE_MEMBERS`, `CPPLS_TRACE_INFERENCE`, `CPPLS_TRACE_ANSWERS`, `CPPLS_TRACE_VIEW`, `CPPLS_TRACE_DIAGNOSTICS`
and the rest, plus `CPPLS_NO_INFERENCE` and `CPPLS_NO_INDEX_LOOKUP` for splitting a measurement into the engine and the
index.

## See also

- [`docs/cpp-support.md`](../../docs/cpp-support.md) — the language support, feature by feature, and the diagnostics
- [`docs/memory-status.md`](../../docs/memory-status.md) — what the analysis holds, measured
- [`docs/module-cache.md`](../../docs/module-cache.md) — the module index and its cache

## License

MIT — see [LICENSE](../../LICENSE).
