# Usability on neovim — measured over the protocol

The question this file answers: **if a reader opens neovim in this server, what works and what does not.** The probe is
`crates/cpp_ls/tests/neovim_usability.rs`: it spawns the real binary, opens files the way a client does, and asks
`textDocument/hover`, `textDocument/definition`, `textDocument/completion` and `textDocument/diagnostic` at positions
taken from the file's own text. Only **uses** are asked about — `name(`, `.name`, `->name`, `::name` — because those
are the questions a reader actually points at.

```text
$env:CPPLS_NEOVIM = "E:\neovim"
$env:CPPLS_USABILITY_EVERY = "23"      # every 23rd source
$env:CPPLS_USABILITY_FILES = "20"      # of 517
$env:CPPLS_USABILITY_POSITIONS = "10"  # uses per file
cargo test --release -p cpp_ls --test neovim_usability -- --ignored --nocapture
```

## The numbers (2026-11-15, release, this machine)

20 of 517 sources, 151 uses, 28 s:

```text
hover:      128/151 answered (85 %), p50 5 ms, p95 92 ms
  arrow    17/17   (100 %)
  call     72/85   (85 %)
  dot      39/49   (80 %)
definition: 121/151 answered (80 %), p50 0 ms, p95 22 ms
  arrow    13/17   (76 %)
  call     69/85   (81 %)
  dot      39/49   (80 %)
completion: 57/66 non-empty (86 %), 2 858 item(s) in total
```

**The latency is not the problem**: a request after the file's first one is 0–5 ms at the median. What costs is the
first request per file, which pays for the reading (92 ms at p95 here, seconds for a file with a large closure).

A second sample of 40 files (every 11th, one use each) asked only for diagnostics:

```text
19 finding(s) over 40 file(s), 3 with at least one error
  src\nvim\eval\vars.c        macro-parentheses×1 undefined-name×3
  src\nvim\map_key_impl.c.h   syntax-error×1
  src\nvim\option.h           missing-return×1
  src\nvim\os\fs.c            include-not-found×4 unused×2
  src\nvim\regexp.c           else-after-return×1 macro-parentheses×3
  src\nvim\spellfile.c        else-after-return×3
```

Half a finding per source file, almost all style — which is the right shape for an editor. The `undefined-name`
cluster is the `globals.h` reading bug below: `vars.c` is one file that uses those globals.

## What does not answer, by category

From the misses of the 20-file run, in order of frequency:

1. **Compiler attributes and the macros that wrap them** — `__attribute__`, `alloc_size`, `no_sanitize`,
   `REAL_FATTR_ALLOC_SIZE`, `FUNC_ATTR_ALLOC_SIZE` (8 of 23 hover misses). `__attribute__((…))` is a GNU extension
   the grammar skips, and a name inside a *macro definition* is not a use the analysis answers for.
2. **Macro-table fragment headers** — `src/nvim/map_*_impl.c.h` writes `KEY_NAME(...)`/`VAL_NAME(...)`, names its
   **includer** defines (`#define KEY_NAME …` around the `#include`). Opened on its own the fragment cannot resolve
   them, and no server can: there is no meaning to the file without the includer.
3. **A member whose type is declared later** — `decoration_defs.h:121` `.virt_lines`, `.data`, `.state` and five
   more: members of a struct field whose type is another struct in the same header, read as its own translation unit.
4. **A call whose declaration lives in a header the file does not include** — `ml_get`, `set_bufref`, `lua_gettop`.
   In the build these come in through another header; the file's own `#include`s do not name them.
5. **A macro-renamed member** — `b_fnum` (`#define b_fnum handle` inside `struct file_buffer`), which only a
   rendering can resolve. This one is already documented as the price of answering from the written text.

## The one defect that is not a category

`globals.h` declares the editor's globals through `EXTERN … INIT(…)` and includes `macros_defs.h` with a path only the
translation unit's `-I` makes reachable. Read on its own the header does not find it, sixteen declarations parse as
expressions, and every name after the first is lost — in the **editor**, not only in the batch checker: hovering
`Visual` in `cursor.c` answers nothing, and the checks underline it as `undefined-name`. That is 135 of the 138
`undefined-name` findings the batch checker reported for this project, and it is the single largest usability defect
measured here. `docs/cpp-check.md` has the measurement and the fix's shape (a header's summary asked for in the
context of the file that included it).

## What to fix next, in order

**Updated 2026-11-16:** item 1 is done (the re-measurement at the end of this file), and the order of what remains is
2, then 3, then 4.

1. **The header context above.** One fix, ~90 % of the false diagnostics on this project, and the missing hovers for
   its globals.
2. **`macro-redefined` on headers** — 11 findings in the 20-file sample, all on headers read as their own unit, where
   a macro table is re-defining names for each includer. A fixture with `#undef` before the second definition stays
   silent, so these need per-file attribution before the rule is trusted on headers.
3. **`include-not-found` on a file with no compile-database entry** — `os/pty_proc_unix.c` and friends are not part of
   a Windows build; the warning is true and useless. Demote it when the file has no command of its own.
4. **The misses that are a reading question, not a bug** — items 2 and 5 above. Documenting them is the honest fix;
   making them resolve needs a rendering per open file, which is the editor's 0.7 s first request.

## Re-measured 2026-11-16, after the unit work

The header reading has changed since the numbers above, and the defect is gone. Measured directly, with `inspect hover`
on the checkout:

```text
cursor.c:104:22  `Visual`  ->  `VisualState Visual`, declared in `globals.h:453:20`
```

— the globals are read, so the 135 `undefined-name` findings that dominated the batch run are no longer the shape of the
project. The fix this document asked for is the one landed in `Session::the_unit_that_reads` (a header is read in the
context of a unit that includes it); **which of the reading changes moved this number is not bisected here**, and the
record of the earlier measurements — `9aaccc40` shows `Visual` still unanswered after its own change — is why the
attribution is left open rather than guessed.

A fresh 20-file sample (every 23rd source, up to 10 uses per file, the probe's own defaults) reads:

```text
diagnostics: 21 finding(s) over 20 file(s), 2 with at least one error
     11  macro-redefined
      3  syntax-error
      2  braces-around-statements
      2  else-after-return
      2  macro-parentheses
      1  undefined-name
completion: 57/66 non-empty (86 %), 2 862 item(s)
```

So fix #1 above is **done**, and the dominant category is now #2 (`macro-redefined` on headers read as their own unit).
The hover misses are the classes *"What does not answer"* already names (compiler attributes and the macros wrapping
them, macro-fragment headers, a member whose type is declared later, a call whose declaration arrives through another
header).
