# `cpp_check` — the analysis's checks, over a whole workspace

```text
$ cpp_check                          # the current directory, `warning` and above
$ cpp_check E:\neovim                # a project
$ cpp_check --min-severity=hint      # style hints too
$ cpp_check --json                   # one JSON object, for CI
$ cpp_check --code=use-after-free,dangling-return
$ cpp_check --skip-code=braces-around-statements
$ cpp_check --headers                # headers on their own, not only translation units
$ cpp_check --max-files=25           # a smoke run
$ cpp_check --exclude=third_party    # a path substring to keep out of the walk
```

Exit code: `1` when at least one finding is at or above `--min-severity`, `0` when none is, `2` for a run that could
not start (no such directory, no sources, an unknown rule name).

## What it is, and what it is not

It is **the server's own run**, batched. One `Session` over the project — the compile database, the toolchain's
include search, `.cppls.toml` — and then, per file, the three steps a client's `didOpen` takes on the server:

```text
did_open(file, text)   the text, in the VFS
catch_up(file)         the file's summary, in the index
cook(file)             the closure rendered, the cooked reading filed
diagnostics(file)      the checks, against the file and its tree
```

So a finding here is a finding the editor shows on the same line, with the same range and the same sentence, and a
rule fixed in the server is fixed here. Nothing in `crates/cpp_check` decides what is wrong with a program.

It is **not** a compiler, and does not try to be one. The compiler is the authority on whether a program builds; this
tool exists for the questions the compiler does not answer. The split, by rule:

| Kind | Rules | Compiler? |
|---|---|---|
| The compiler's own errors | `syntax-error`, `undefined-name`, `undefined-field`, `assign-type-check`, `param-type-check`, `argument-count`, `return-type-mismatch`, `missing-return`, `access-invisible`, `redefinition`, `override-nonexistent`, `override-of-final`, `final-class-derived`, `abstract-class-instantiated`, `concept-not-satisfied` | yes — if the translation unit compiles, with the right flags |
| Style and maintainability | `braces-around-statements`, `else-after-return`, `use-nullptr`, `use-auto`, `cstyle-cast`, `simplify-boolean-expr`, `implicit-bool-conversion`, `macro-parentheses`, `unused` | no (clang-tidy, but not in the editor) |
| **Workspace-wide** | `pure-virtual-not-overridden`, `unused`, `missing-include`, `include-not-found`, `ue-naming`, `ue-specifier`, `ue-generated-header`, `ue-generate-body`, `ue-macro-scope` | **no** — a compiler sees one translation unit |
| **Danger patterns** | `dangling-return`, `dangling-field`, `use-after-free` | no — these compile and then break |

The four rows are ordered by what a check run should be trusted with. The first is the *weakest* argument for the tool:
the compiler is more precise, and its answer arrives as soon as the build is green. The value is in the last two rows,
and in the files a build never sees — a header in isolation (`--headers`), a branch `#ifdef`-ed out on this platform,
a directory the build system does not own, a tree whose build is broken right now.

## The reading step is the cost

Every file is cooked before it is asked about, because a check that resolves names against a file whose macros are
unexpanded is a check about a program nobody compiles. The closure is read once per session: a header read for the
first file is a summary the rest of the run already has.

`index_everything` looks like the batch version of the reading step and is **not**: on a real project it spins —
measured on neovim, **seventeen seconds of CPU in sixteen minutes** with the session making no progress, which is the
"pending 0, cooking N on every round" failure `Session::index_everything`'s own comments describe. The server never
drains that queue in one call either; it pumps in slices under a budget, and so does this tool.

The checks are also **gated**: a check may only publish once every header a file includes unconditionally has been
read (`Session::is_closure_ready`), because a check reads absence out of the index. The tool pumps until that gate
opens, bounded so a header that cannot be read at all cannot stall the run; a file that never opens it is counted in
the summary as *withheld* rather than reported as clean.

## Measured

Release build, this machine, `cpp_check E:\neovim --min-severity=hint --quiet`, 2026-11-14:

```text
files       228 sources discovered and checked (the project has 517 files including headers)
time        274.8 s (~1.2 s per file, one session, shared headers)
memory      ~650 MB resident
findings    446
parse errors 4
```

By code, and what each one turned out to be:

| code | count | verdict |
|---|---:|---|
| `undefined-name` | 138 | **135 false**, all in `src/nvim` — the header-context gap below |
| `else-after-return` | 110 | true, style |
| `braces-around-statements` | 81 | true, style |
| `macro-parentheses` | 42 | true in form; the macros in `src/xxd` are used as constants |
| `include-not-found` | 28 | **platform**, not defects: neovim is a POSIX project checked on Windows (`<termios.h>` …) |
| `redefinition` | 14 | false on the ones read: `xfree(*matches)` is a *dereference* of a parameter, not a second declaration |
| `assign-type-check` | 11 | false on the one read: `const char *p = strchr(...)` — `char*` converts to `const char*` |
| `return-type-mismatch` | 5 | false on the ones read: a macro's `0` inside a statement read as a return value |
| `unused` | 5 | true |
| `missing-include` | 3 | true in form (`Dict` from `keysets_defs.h`) |
| `simplify-boolean-expr` | 3 | true, style |
| `missing-return` | 2 | unverified |
| `syntax-error` | 4 | in intentionally broken test fixtures (`test/functional/fixtures`) |

**43 % of the findings are style hints**; every correctness finding sampled was a false positive. The tool is therefore
**not a CI gate yet** — but it is a scoreboard, and the list of what to fix is exactly the false positives above, in
order of volume.

## The header-context gap — why `undefined-name` fires 135 times on code that compiles

`globals.h` declares the editor's globals through two macros:

```c
EXTERN VisualState Visual INIT( = { .mode = 'v' });      // globals.h:453
```

`EXTERN` and `INIT` are defined in `macros_defs.h`, which `globals.h` includes as `"nvim/macros_defs.h"` — a path that
resolves only with the translation unit's `-I` flags. **A header has no configuration of its own**, so a standalone
read of `globals.h` finds no `macros_defs.h`, sees `EXTERN` and `INIT` as plain names, and parses sixteen lines as
expressions:

```text
globals.h:140:8  syntax-error  expected `;` after expression     EXTERN colnr_T dollar_vcol INIT( = -1);
globals.h:301:8  syntax-error  expected `;` after expression     EXTERN sctx_T current_sctx INIT( = { 0, 0, 0, 0 });
```

Every declaration after the first such line is lost, so `Visual`, `typebuf`, `msg_grid` — real globals, declared in
that header — come back `NotDeclaredHere` from every file that uses them. The engine files a header's summary **per
context** (`SummaryKey` carries a context hash), so the fix is not to guess flags: it is to make the query that
resolves a name through a header ask for the summary **in the context of the file doing the asking** — the one whose
closure was just cooked. Until then, a check run on any project that declares its globals this way (vim, neovim, and
most C programs of that vintage) will report them all as undefined.

That is one engine fix, and it is worth more than every rule in the tool: it removes 98 % of this project's findings
and the editor has the same false positive in it.

## Known limits

* A header checked on its own (`--headers`) is read as a translation unit, and some headers are fragments — pieces of
  a declaration another file opens. The analysis has a rule for the loudest case (`#error` plus a body that starts
  with `{` or `}`) and reports one message instead of hundreds; the rest is the reader's judgement.
* A file whose include closure cannot be resolved (no compile database, no toolchain) gets the parser's errors and
  the checks that do not need types; the summary says how many files that was.
* The findings are the analysis's, with the analysis's refusal to guess: a rule that cannot decide says nothing.
  `--min-severity=error` is therefore not "what the compiler would say" — it is "what this layer is sure of".
