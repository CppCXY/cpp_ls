# Diagnostics performance, measured through Neovim

**The question.** A reader's editor spends a core on diagnostics while typing, and the gutter updates
seconds after the keystroke. This file is what was measured, how, and where the time actually goes —
with Neovim 0.11.3 as the client, because that is where the report came from and because its pull
implementation is the one this server's `diagnosticProvider` turns on.

Everything here is reproducible from `target/bench/` (gitignored): `gen.py`, `lspbench.py`, `sweep.py`,
`nvim_e2e.lua`, `nvim_set_cost.lua`, `real.py`, `flood.py`, `split.py`, `narrow.py`, `querycost.py`.
The exact commands are in [Reproducing](#reproducing).

## 1. What a keystroke actually sends

| step | who | what |
| --- | --- | --- |
| 1 | server (`initialize`) | advertises `diagnosticProvider` — see `handlers::diagnostic::DiagnosticCapabilities` |
| 2 | Neovim | `vim.lsp._set_defaults` sees `client:supports_method('textDocument/diagnostic')` and calls `vim.lsp.diagnostic._enable(bufnr)` (`vim/lsp.lua:840`) |
| 3 | Neovim | `_enable` registers an `LspNotify` autocmd on `textDocument/didChange` **and** `didOpen` (`vim/lsp/diagnostic.lua:361`) |
| 4 | Neovim | the autocmd calls `_refresh(bufnr, { only_visible = true })`: it **cancels the pull still pending**, then sends `textDocument/diagnostic` with `range` = the window's lines (`util.lua:2214`) |
| 5 | server | `on_pull_document_diagnostic` **ignores `params.range`**, runs `prepare_for_a_diagnostic` and `diagnose_file` for the whole file, and answers a full report |
| 6 | Neovim | `on_diagnostic` → `vim.diagnostic.set(namespace, bufnr, items)` — this **replaces every extmark in the namespace** |

Two consequences of step 1 are worth stating because they are not visible from the server code:

* **The push path is off for Neovim.** `DiagnosticService::schedule_open_files` returns immediately when
  `supports_pull_diagnostic()`, and Neovim advertises `textDocument.diagnostic` in
  `protocol.make_client_capabilities()` unconditionally. So `publishDiagnostics` is never sent to
  Neovim, and the 300 ms push debounce does not apply to it: **a pull has no debounce at all**.
* **The server's "ask again" is inert.** `ClientProxy::refresh_diagnostics` sends
  `workspace/diagnostic/refresh`, and Neovim's runtime has no handler for it (only
  `textDocument/publishDiagnostics` and `textDocument/diagnostic` are mapped in `handlers.lua`). The
  request is answered with an error and dropped. Staleness is repaired only by the next keystroke.

The server does try to have the answer ready before the request: after a slice commits a reading and
the update queue is empty, the pump calls `Session::warm_the_pulls` (`handlers::initialized:654`),
which runs the same `diagnostics()` call for up to two open documents. That is why the pull itself
usually answers in ~1 ms — **the work has already happened on the pump thread**, which is also why
the *next* `didChange` can be seen waiting behind it (`apply_upto: event 4 took 250 ms`).

## 2. Where the time goes

### 2.1 One pull after one edit, by file

Isolated directory per size (so the project the server indexes is exactly one file), one-character
insertion, pull issued synchronously (`lspbench.py --mode sync`). "cold" = the pull after `didOpen`,
"edit" = the pull after the edit, "warm" = a pull with no edit in between (memo hit).

| file | lines | cold | edit | warm | diagnostics |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean, 25 classes | 226 | 475 ms | 222 ms | 0.7 ms | 0 |
| clean, 50 | 451 | 972 ms | 840 ms | 0.4 ms | 0 |
| clean, 100 | 901 | 3 467 ms | 3 321 ms | 0.6 ms | 0 |
| clean, 200 | 1 801 | 13 200 ms | 13 002 ms | 0.8 ms | 0 |
| clean, 300 | 2 701 | 29 993 ms | 29 508 ms | 1.3 ms | 0 |
| errors, 25 | 235 | 260 ms | 6.7 ms | 0.2 ms | 9 |
| errors, 100 | 935 | 150 ms | 21 ms | 0.6 ms | 34 |
| errors, 300 | 2 801 | 207 ms | 49 ms | 1.3 ms | 100 |

Doubling the file roughly quadruples the cost: **the pull path is quadratic in the file's size** for a
file that declares its own types. A file of the same size whose bodies do not parse is hundreds of
times cheaper — see §3.4.

### 2.2 The same thing end to end, inside Neovim

`nvim --headless -u NONE -l nvim_e2e.lua … <edits>`: latency from `nvim_buf_set_text` to the pull's
`vim.diagnostic.set` having run.

| file | first report | per keystroke (median) | `vim.diagnostic.set` |
| --- | ---: | ---: | ---: |
| clean, 901 lines | 5 281 ms | 4 427 ms | 0.1 ms |
| errors, 2 801 lines, 100 diagnostics | 552 ms | 302 ms | 1.9 ms |

The server log for the second row, one keystroke:

```text
apply_upto: event 4 took 250 ms
catch_up_upto: waited 249 ms for events through 4
pull prepare: 1 file(s) in 35 ms, … — no cook
request textDocument/diagnostic waited 249.849ms for events and took 48.6397ms
```

So of the ~300 ms, **250 ms is waiting for the update queue to drain** (the pump is still busy with
the previous slice) and 49 ms is the pull. The client's own work is 1.9 ms.

### 2.3 Stage split inside one pull

From the server's own `debug` lines (`--log-level debug`):

```text
pull prepare: 2 file(s) in 21.5ms, check 46µs, gather 11.6ms, render 17.3ms, commit 1.43ms (true)
diagnose … 0 error(s) … 0 check(s), checks withheld false
a query took 3329 ms — permit 0 ms, gate 0 ms, session 0 ms, ran 3329 ms
```

The prepare is *cheap* (50 ms). The 3.3 s is inside `Session::diagnostics` → `checks_about` → the
semantic checks. For a realistic 35-line translation unit that includes `<vector>`, `<string>`,
`<map>` and `<algorithm>` the split is the opposite — the checks are withheld (closure not ready) and
the cost is the cook's commit:

```text
pull prepare: 1 file(s) in 1.9ms, check 15.9µs, gather 85.7µs, render 511.7µs, commit 279ms (true)
```

First pull 164–272 ms, pull after an edit 207–301 ms, for **35 lines of source**.

### 2.4 The checks are the quadratic term

`Session::checks_about` builds `SemanticModel` and runs all ~40 rules (`sema::check::run`). Each rule
that reasons about a type reaches `type_of`/`members_of`, and those query paths rebuild
`DeclarationShapes::of(root)` — **a pass over every node of the file** — per call:

| query | 200-line file | 901-line file |
| --- | ---: | ---: |
| one `members_of` (miss) | 8.2 ms | 29.5 ms |
| one `DeclarationShapes::of` | 1.3 ms (1 000 shapes) | 3.6 ms (2 000 shapes) |

Instrumented for one pass over the 901-line file (temporary `eprintln!` behind `CPPLS_TRACE_TIME`,
reverted):

```text
[shapes-of] 1801 builds, 6 587 ms total       (wall 7 176 ms)
[member]    100 queries, 2 954 ms total       (~8 builds each)
```

**91 % of the diagnostic pass is rebuilding one file's declaration table over and over.** The call
sites are exactly the ones `DeclarationShapes::of`'s own doc comment names as the problem:
`declared_type_of`, `declared_alias_target`, `declared_returns_of`, `declared_template_parameters`,
`declared_parameter_defaults`, `declared_bases_of` (`sema/declarations.rs:109, 1274, 1416, 3280, 3289,
3428`), plus `index/project/members.rs:1815, 1853` and `index/project/bases.rs:254`. The `_with`
variants that take a table already exist; the query layer does not have one to pass.
`SemanticModel` holds one (`semantic.rs:569`), but `members_of`/`bases_of` are not reached through it.

The same program with the classes declared in a **header** instead of the file being checked costs
**96 ms** instead of **1 361 ms** (100 classes): a class from the index takes the
`index.declarations_in` arm, which walks no tree, and `direct_members` never reaches the
`DeclarationShapes::of` arm at all.

### 2.5 A flood of diagnostics is not the problem

8 000 lines, 2 000 parse errors: first pull 308 ms, pull after an edit 72 ms, 2 000 items in a
**387 KB** JSON response. Parse errors are linear and cheap; it is *semantic* checks over a valid file
that cost seconds.

## 3. What Neovim does with the answer

### 3.1 `vim.diagnostic.set` is ~13 µs per diagnostic

`nvim_set_cost.lua`, five runs each, median (headless; no UI, so no redraw):

| diagnostics | default config | no virtual text | no signs | severity_sort |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 1.1 ms | 2.2 ms | 2.6 ms | 4.1 ms |
| 1 000 | 11.1 ms | 12.3 ms | 15.1 ms | 13.9 ms |
| 5 000 | 64.0 ms | 70.8 ms | 70.7 ms | 69.0 ms |
| 10 000 | 133 ms | 136 ms | 139 ms | 148 ms |

Linear, and the display options barely matter in a headless run — the work is the extmarks. For the
100-diagnostic file above this is 1.9 ms, so **the client is not the bottleneck**; at 10 000
diagnostics it would be 133 ms *per keystroke*, which is where a client-side limit would matter.

### 3.2 `update_in_insert = false` (the default) only defers the drawing

`vim.diagnostic.set` stores the diagnostics and calls `show`; `show` schedules the display when the
mode is insert (`vim/diagnostic.lua:2121`). The request has already been sent, the server has already
paid for the answer, and the LSP→vim conversion has already run. What is deferred is the extmarks —
and they are then placed in one batch when insert mode ends.

### 3.3 The rest of the per-keystroke traffic

* **Semantic tokens**: also re-requested on `didChange`/`didOpen`, debounced 200 ms
  (`vim/lsp/semantic_tokens.lua:642`). On the 901-line file one such request took **2.87 s** on the
  server (`request textDocument/semanticTokens/full … took 2.8679761s`) — the same checks path, a
  different answer.
* **Change coalescing**: eight `nvim_buf_set_text` calls in one event-loop turn produced **two**
  `didChange` notifications and two pulls (one cancelled). At human typing speed it is one per
  keystroke: three edits produced four `vim.diagnostic.set` calls (didOpen + 3).

### 3.4 Why the error file is fast and the valid file is slow

There are two gates. The first is `checks_withheld` in `session/diagnostics.rs`: while the file's
closure is not ready the checks do not run at all — that is the realistic 35-line TU above, where the
standard headers are still being read. The second is the parser's own damage: in a body the parser
could not finish (`sum = sum + ;`), every type question the checks ask answers `Unknown` before it
reaches a declaration, so the 8 ms member queries never happen. A **clean** file is the expensive
case because every function body is inferred, and each class-typed declaration asks for a member list.
That is the counter-intuitive part of the report: **a file full of syntax errors is cheap to
diagnose, and a valid file with its own class definitions is the expensive one.**

## 4. What was changed, and what it bought

**One declaration table per request, threaded.** `sema::declarations::ShapeTable` is a
`OnceCell<DeclarationShapes>` that a caller owns for the length of a request, and
`shapes_from(&table, root)` builds the table on the first ask and lends it afterwards.
`SemanticModel` owns one (`shapes`), and the queries that can need it take `&ShapeTable`:

```text
members_of_with, own_members_of_with, members_of_inner, direct_members,
the_base_clauses_of, bases_of_with, bases_of_inner,
member_fact, member_fact_inner, direct_member, fact_from_binding_with,
class_template_parameters, resolve_aliases*, alias_target_of,
what_a_call_has, type_of_expression_memoed, type_of_expression_inner,
declaration_of_a_callee, type_of_a_call, callee_of_a_call
```

Every public entry point keeps its old signature as a wrapper that hands in a fresh table, so the
callers with no request to share one in (completion, hover, tests) are unchanged — they build what
they built before, and the hot path builds one table instead of 1 801.

Two smaller findings came out of the same measurement and are fixed here:

* `fact_from_binding_with`'s `declared_returns_of_in(…).or_else(declared_returns_of(…))` fallback was
  building a whole table to ask a question `_in` had already answered — 1 100 builds of one 901-line
  pass. The fallback is now `declared_returns_of_with(shapes, binding)`, which answers the same thing
  (the wrapper's one extra case is the operator spelling that `_in` refuses) without a tree walk.
* The inference engine's `declared_returns_of` / `declared_template_parameters_of` /
  `declared_parameter_defaults_of` calls inside `what_a_call_has` are now the `_with` forms; those were
  the other 1 000 builds.

**And the semantic-token pass, which was the same bug in a different loop.** `kind_of_binding` — asked
once per declaration, 1 100 of them in the 901-line file — called `declared_template_parameters_of` and
`declared_type_of`, whose wrappers build a table each, and `member_kinds` called `member_across_files`
once per member access with a fresh `TypeMemo` and a fresh table. `classified_names` now owns the table
(and a `TypeMemo`) for the whole file and hands both to `member_kinds` and `kind_of_binding`; the two
`member_across_files` callers are split into a `_with` form. That is 1 100 tables down to one.

**And `access-invisible` was asking for a member list it could never use.** On a 35-line file that
includes `<vector>`, `items_.size()` asked for `std::vector`'s whole member list — ten bases and the
walk that walks them — to find a `size` whose declaration is not in this file, which is exactly what
the rule documents it does not judge. Measured: **263 ms of a 271 ms diagnostic pass**, the single most
expensive rule. The rule now asks the object's class first (`scope_with_qualified_name`, which is
`direct_members`' own first question) and returns unless this file declares it; the same pass is 1 ms.
The next rule to take the 263 ms was `argument-count`, on `std::map<std::string, int>` — one list build
of 132 members, and it is paid **once**: the index's `member_lists` cache serves it to the next pull, so
the first pass after opening a real TU costs ~240 ms and the ones after it 5–10 ms (§5).

Measured, same machine, same files as §2:

| | before | after | |
| --- | ---: | ---: | ---: |
| pull after an edit, 901-line file | 3 321 ms | **115 ms** | 29× |
| `cpp_check`, 901-line file | 7 222 ms | **277 ms** | 26× |
| `cpp_check`, 2 701-line file | 49 561 ms | **1 479 ms** | 33× |
| semantic tokens, 901-line file | 1 150 ms | **8.8 ms** | 130× |
| realistic TU (35 lines, 4 standard headers), pull after an edit | 207–301 ms | **5–10 ms** (first pass 239 ms) | 30× |
| Neovim per keystroke, 901-line file | 4 427 ms | **127 ms** | 35× |
| Neovim per keystroke, 2 801-line file | 302 ms | **101 ms** | 3× |
| Neovim per keystroke, realistic TU | ~250 ms | **158 ms** | 1.6× |
| table builds in one pull, 901-line file | 1 801 | **2** (the indexer's own) | |

The Neovim rows are the point of the exercise: the pull was already 113 ms once the table was shared,
and the other 1 s was the update queue waiting for the semantic-token request the editor sends 200 ms
after every change — the same per-declaration table builds, on the same analysis thread. With that gone
a keystroke on the 901-line file costs ~127 ms end to end. The realistic TU's 158 ms is now mostly the
didChange itself — the cook of a translation unit that expands four standard headers, which §4's
"what is left" item 2 is about.

### What is left, in order of expected gain

1. **Memoise type inference per view.** The inference engine now shares the table but still re-answers
   `type_of` for the same declarations across rules; the `TypeMemo` is the place, and it already exists.
2. **Make `commit_a_cooked` proportional to what changed.** 279 ms for a 35-line file means the commit
   is paying for the whole cooked rendering (the standard headers it expands), every keystroke — and
   the log above still shows a `commit` of 1.09 s on the first pull after `didOpen`.
3. **Only then consider a pull-side debounce.** A 100 ms coalescing window on `textDocument/diagnostic`
   would cut the number of passes during fast typing, but it adds latency to every edit and hides the
   real cost rather than fixing it. The push path's 300 ms delay exists because push has no
   cancellation; pull has cancellation and does not need the same delay.

Client-side guidance that does **not** help: `signs`, `virtual_text`, `severity_sort` and
`update_in_insert` are all in the noise against a 3 s server, and none of them stops the request.
`vim.diagnostic.config({ update_in_insert = false })` is already the default.

## 5. Neovim behaviours worth knowing (not fixable here)

* **Neovim sets the whole namespace for a range request.** `_refresh` asks for the visible range, but
  `on_diagnostic` → `vim.diagnostic.set` replaces every diagnostic of that client in the buffer. A
  server that honoured `range` strictly would make off-screen diagnostics vanish — which is why this
  server ignoring the range is the pragmatic choice, and why it cannot use the range to reduce work.
* **`workspace/diagnostic/refresh` has no handler** in Neovim 0.11.3's runtime, though the server
  sends it. Diagnostics that change because a *header* changed are not re-requested until the next
  `didChange` in the buffer.
* **Requests are cancelled, not queued.** `_refresh` issues `$/cancelRequest` for a pending pull before
  sending the next one; the server answers `Cancelled` at the next await point, and the work already
  spent is lost. During a burst this is most of the work.

## Reproducing

```powershell
# one file, one edit, timing per stage
python target/bench/lspbench.py target/release/cpp_ls.exe my.cpp --root . --edits 5 --warm 3 --mode sync

# the same inside Neovim (headless), with the server's debug log
nvim --headless -u NONE -l target/bench/nvim_e2e.lua target/release/cpp_ls.exe . my.cpp 5 0 "--log-level debug"

# the client's own cost
nvim --headless -u NONE -l target/bench/nvim_set_cost.lua

# the size sweep (one isolated project per size)
python target/bench/sweep.py
```

The `[shapes-of]` / `[shapes]` counts in §2.4 come from temporary `eprintln!`s behind
`CPPLS_TRACE_TIME` in `DeclarationShapes::of` and `direct_members`; they were removed after the
measurement, and the numbers above are the ones they printed.

## Open questions

* How much of a real project's per-keystroke cost is `warm_the_pulls` on the pump thread versus the
  pull itself: the two are hard to separate in a log that also carries indexing slices.
* Whether a shapes table can be keyed cheaply enough to be correct. The tree is per-view and
  ephemeral; the key has to be the tree's identity plus the revision that produced it, not a path.
* Whether the single-slot `checks_memo` (one file's findings, per session) thrashes when a reader
  alternates between two files — not measured here.
