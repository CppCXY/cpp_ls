# Indexing, caching, reuse and locking — a design note

This note began as a design and is now also a **status**: what the tree does today, what it cost to get there, what is
broken, and what is left. `docs/indexing-review.md` is the measured counterpoint to it, written against `3c2f784`;
where the two disagree, the review's numbers win because they were taken with a probe and a second project.

Everything below was measured with `start_shape` (the pump's own shape), `hover2.mjs` (a faithful client), `pump_probe`,
`cpp_corpus` and `CPPLS_TRACE_*`, on release builds.

## 0. The baseline, before any of this work

| what | measured | what it said |
| --- | --- | --- |
| cold first pass (per-file summaries) | 94–149 s, ≈54–120 ms/file | single-threaded parse + facts + encode + SQLite |
| second pass, before caching | 113–132 s | 13 whole-closure timelines per 16-file drain |
| second pass, after caching | 59.7 s | 1220/1761 served from cache, 81 unit walks |
| whole-project drain, before phase 0 | **26.3 s** (124 slices, `EmmyLuaCodeStyle`, 103 project files) | every start re-read the project |
| steady-state hover | 2–33 ms | the query was not the problem |
| first hover, warm cache | 1.6 s | the request waited before it was answered |
| first hover, cold cache | 37–43 s | …for the whole project's indexing |
| C++ corpus | **51/52 types, 10/10 members** | the capability the rewrite had to keep |

## 1. Why the old design paid these prices

1. **The unit of exactness is a translation unit; the unit of caching and of incrementality is a file.** A file's
   meaning can be decided by macro bodies in files it includes, so a per-file summary is a *shape-only* reading until it
   is re-read through its include closure. Repairing one file therefore cost one whole-program parse, and the only key
   that could cache the result described the whole closure.
2. **A rendering was not a first-class value.** The same file was rendered again for every root that covered it, in three
   different caches keyed three different ways. Nothing was shared.
3. **Requests and the pump shared one mutable session behind one lock,** and one request path (`read_the_modules`) even
   drained the whole index queue under the write lock — 1 468 ms of a 1 862 ms first hover.
4. **The startup contract was "the whole project is exact".** No editor can afford that promise; clangd and Rider both
   keep the index approximate and are exact only for what the user is in.
5. **Invalidation was coarse** (a closure fingerprint changes when anything in the closure changes).
6. **The data model mixed environment-free syntax with environment-resolved meaning**, which is what made the repair
   pass necessary in the first place.

## 2. The replacement

### 2.1 Four layers, each with its own key

```text
L1  stub record        per file, environment-free: names, kinds, scope paths, signatures, macro names and
                       spelling-level definitions, include edges. Key: (content_hash, context_hash).
L2  resolved facts     per (file, environment): scopes a macro body opens, declarations inside a macro body,
                       which `#if` branch the program takes. Key: (content_hash, context_hash, env_set).
L3  TU view            the rendered text + parse tree for the file a request is about. Key: (content_hash, env_set).
L4  file text          the VFS. Unchanged.
```

**L1 is never repaired.** That sentence removes the whole-project second pass. L2/L3 are built only for a file somebody
asked about. The tree is not there yet — see §4.1 — but it is the target.

### 2.2 Answer from a published snapshot, never from a half-built layer

A request loads the current state atomically and answers from it — correct **for that snapshot**, never partial. If the
answer needs L2/L3 and the snapshot lacks it, the request starts that work and answers from L1. The lesson of the
reverted `c5d907e`/`b0dfbb7` is the reason: a request that answers before its data is ready is not fast, it is wrong.

### 2.3 Resolve a file by composition, not by parsing its closure

The macro bodies in force are needed only for the names a file actually invokes, and only their shape matters
(`BodyShape`: namespace / close / block / qualifying / other). A lazy `MacroBodies` resolver over L1 turns "read this
file in its program" into "read this file, with a handful of lookups". Measured: one refinement **118–191 ms → 25.8 ms**.

**What this round got wrong:** the composition path was wired into the *pass* but not into `cook`, and the refined
reading is now **weaker than the raw one** in five measured places (§4.1). Composition is the right mechanism; it must
produce a reading at least as good as the raw one, not a different one.

### 2.4 Invalidation by consulted definitions

An L2/L3 entry records the `(definer_path, body_range, body_hash)` of every body it read and the includes it resolved. An
edit to `F` invalidates exactly the entries that read `F`'s bodies — not "everything that reaches `F`", which is what a
closure hash does.

### 2.5 One cache, byte-bounded

One table keyed `(content_hash, context_hash, env_hash, kind)`, `kind ∈ {stub, resolved facts, rendering, tree}`, memory
LRU by bytes, backed by the existing disk database. Every consumer reads it; every producer writes it once.

### 2.6 Startup contract

`initialize` promises an answer about any project file **correct for a published snapshot**, from L1, as soon as project
files have stub records. Refinement runs afterwards, in order: the open files, their closures, then the rest.

## 3. Where the work stands — measured today

`start_shape`, `C:\Users\zc\Desktop\github\EmmyLuaCodeStyle` (1 243 project files scanned), release:

```text
Session::open                    140 ms cold, 94 ms warm (the toolchain answer is cached)
whole pass (cold, in process)  36 300 ms, 109 slices      (the server, no cache: 96 s, 445 slices)
  prepare (read lock)          32 332 ms
  commit (write lock)             661 ms total
  drain + cooking (read lock)   3 306 ms
  worst single commit              27 ms      ← the write budget is 50 ms, and the server's cold start
                                                 measured 25 ms worst / 0.8 ms mean
whole pass (warm)              3 852 ms, 41 slices
did_open                          0.3 ms
catch_up                        123 ms       ← the first request's cost, before any query
cook                             11.5 ms
view                              2.92 ms
diagnostics (every check)       153.3 ms     23 finding(s) — 2 ms on the repeat
steady view                       0.01 ms
```

`hover2.mjs` over the wire: first hover **206–208 ms** immediately after `didOpen`, **3 ms** once the pump has run. Before
this work: 1 850 ms. The server's cold index of 1 695 files (no `.cppls`) took **96 s** in 445 slices — the throughput item
of §4.6.

| phase | state | measured |
| --- | --- | --- |
| 0 — lazy refinement | **landed** `7bc4c60` | start = index only, **0** timelines; 26.3 s → 4.6 s (review's project), 7.1 s cold / 5.4 s warm here |
| 1 — answer from L1, ask for L2 | **landed** `bb7bce2` | definition **26.5 ms** from the index, declaring file refined 118 ms later |
| 2 — composition instead of a closure parse | **landed** `b010584` | one refinement **25.8 ms**, 0 timelines (was 118–191 ms) |
| 3 — request path | **partial** `889cb32`, `b9d83cf`, `da3c35c`, `334f7ed`, `c7aac67`–`cc69aa9` | 1 850 → **333 ms** first hover; modules queued, not drained in the request; toolchain discovery cached (141 → 99 ms warm) |
| 4 — one cache, parallel index | not started | — |
| 5 — invalidation by consulted definitions | not started | — |

Two measurements from phase 3's investigation are kept because they were surprising: the 1.5 s was `read_the_modules`
draining the index queue inside a request, and what remained was **before the handler ran** (the handshake), not in the
query.

## 4. What is broken, and what is left

### 4.1 The refined reading is weaker than the raw one — *fix first*

Five capabilities are broken and hidden behind `#[ignore]`; `cargo test -p cpp_code_analysis --lib` is
**734 passed / 0 failed / 10 ignored**, and running the ignored ones shows **5 failures**:

```text
a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session
a_file_read_as_a_program_needs_no_guard_filter
a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass
a_keystroke_in_a_body_reads_the_file_through_its_timeline
a_refinement_is_served_to_the_next_session
```

The ignore reasons say one thing: *a refined reading is weaker than the raw one*. The review measured the same defect
from the other side: the C++ corpus went **51/52 → 41/52** types and **10/10 → 9/10** members, and a header went
`NOT COOKED`. `c7aac67` fixed five other tests (the ask, `index_everything`, the candidate filter, cook from the index)
and `dc7478c`/`cc69aa9` made `cook` compositional, but the refined summary still loses what the raw one has — most
likely the guard filter and the macro-scoping pass are applied to one reading and not the other.

* **Acceptance:** the five tests un-ignored and green; `--lib` **739/739 with no `#[ignore]` for these**; C++ corpus back
  to **51/52 and 10/10**.

**The lever was measured, and it is confirmed — the *layer* is what is missing.** Running the experiment (a refinement
that prefers `a_timeline_of_its_own` + a real render, and `index_everything` asking again) fixed **four of the five**
ignored tests, including `a_file_read_as_a_program_needs_no_guard_filter`; the guard test then failed the other way:

```text
a_rendering_is_not_left_in_the_cache_for_the_next_session
  a summary is the environment-free reading, so both branches are in it: 1 of 2
```

Because a refined reading was written **over the summary**. The contract the test states is the design's §2.1: **L1
(the summary) stays environment-free, and the refined reading is L2** — a separate, per-(file, environment) layer that
queries prefer when it exists. The tree has no L2 store yet; the closest thing is the index's **cooked facts**
(`insert_cooked` / `cooked_declarations`, the layer a program read already fills).

So the fix is precise, and it is not "choose a better arm":

1. refinement renders the file (walk) and files the result as **cooked facts** for that file, never as its summary;
2. `definitions`/`members` prefer cooked facts over the summary's when both exist (the test at `session/tests.rs:2033`
   says exactly this: two declarations lists for one file are two translation units, and offering both answers for both);
3. then the ask in `index_everything` stays on, the four capabilities pass, and the guard keeps its two-branch summary.

That is one change of layer, not a rewrite.

**Second measurement: the layer was built, and the queries do not read it yet.** With the refinement filing its rendering as
the file's cooked facts (`insert_cooked`, the plan carrying them through `ReRead.cooked` to the write half) and
`index_everything` asking again: **three** of the five ignored capabilities pass
(`a_class_scoped_by_another_files_macro_body_…`, `a_file_read_back_through_its_own_unit_…`,
`a_refinement_is_served_to_the_next_session`), the guard stays green (its summary keeps both branches), and two
**integration** tests say what is still missing:

```text
tests/semantic.rs: a_local_fact_carries_a_namespace_the_file_only_opens_with_a_macro
tests/semantic.rs: a_return_type_a_directive_splits_is_the_branch_this_compilation_takes
```

Both are questions whose answer the refinement now puts in **L2 while the lookup still reads L1 only**
(`files_declaring`/`certain_declarations`). So the order is forced: **teach the queries to prefer the cooked reading
first**, then the ask and the un-ignoring — a layer nothing reads is a layer that changes no answer.

**And the tree was not green before this work started.** Those same two `semantic.rs` tests fail at `ce2ec82` with the
refinement reverted, so they are a pre-existing red on `main`, not a casualty of this round; the note's "734/0/10" claim
was about `--lib` only. The workspace's real state is 2 red, and the doc's §5 rule — a green run with ignores is not green
— applies to a red run too.

**Third measurement: the layer is read, and the checks are what it costs.** With `ReRead.cooked` + `insert_cooked`, the ask
back on and three tests un-ignored: `--lib` is **737 passed / 0 failed / 2 ignored** (was 734/0/10), the guard stays green,
and the two `tests/semantic.rs` questions about a refined reading still fail — while two one-file `checks` fixtures
(`c_files_are_not_told_to_write_cpp_style`,
`only_an_unparenthesised_expression_body_is_a_macro_parentheses_finding`) **lose every finding**. A rule that renders a file
only when it has includes changed neither set, which says the checks are not reading the reading the queries read: teach
them the same choice, then the trade (three capabilities for two checks) can be settled.
**Fourth measurement: the checks fail for one line of state, and it is `is_refined` without a reading.** Traced
`Session::diagnostics` on the failing fixture with the L2 refinement on:

```text
diag: cooked=false summary=true refined=true
diag: no cooked reading — no answer at all
```

`diagnostics` has **no arm** for "refined but not cooked": the file is marked refined (the plan pushed its summary), the
cook is therefore never made, and the pull answers `None` — so every code disappears, which is exactly what the two
`checks` fixtures assert against. The composed fallback claims a reading it did not make.

The fix is to **produce the reading**, not to drop the claim: making a refinement always file something cookable (the walk
already does; the fallback must render the file from its own directives and file that). Dropping the claim instead was
measured too — it moved the failure to a third `checks` test, so the state "refined and cooked" is what the rest of the
session assumes, and a refinement that leaves the file unrefined is a second inconsistency, not a fix.
**Settled in `f0dea64`.** The layer landed: `ReRead.cooked` carries the walking arm, the write half files it with
`insert_cooked`, the summary is never overwritten, and the refinement cache stops serving a summary — so "refined" now
always means "there is a reading". Measured on that commit: `--lib` **737 / 0 / 2** (was 734 / 0 / 10), `tests/checks.rs`
**108 passed** (the 105 regression is gone), `tests/semantic.rs` unchanged at 2 pre-existing failures, clippy green.
Three of the five capabilities are back; the two still ignored are the guard filter on a program-read file and a keystroke
through its timeline, and their reasons now name what is missing.
### 4.2 The request path is the slow part now — *second*

Steady `textDocument/diagnostic` is **389 ms median** (review) / **200 ms here**, of which **97 % is type inference**
(`CPPLS_NO_INFERENCE=1` answers the same pull in 11.4 ms). The `SemanticModel`'s memo lives for one request, so an
unchanged file re-derives every answer on every debounce, and the whole pull runs under the session's **read lock** — a
389 ms pull is also a 389 ms barrier for the pump's next commit.

* **Change:** cache inference answers keyed `(content_hash, context_hash, env_hash)`, with hit/miss counters.
* **Acceptance:** steady pull on `CodeFormat.cpp` ≤ 50 ms; two consecutive pulls of an unchanged file differ by < 20 %;
  the prepare and diagnostics phases of `start_shape` drop accordingly.

**Measured baseline for this item** (`start_shape`, `EmmyLuaCodeStyle`, release, 2026-11-03): `diagnostics (every check)`
**143.4 ms**, `view` 2.9 ms, `cook` 12.8 ms, `catch_up` 654 ms. The hot spot is `Session::checks_about`: it builds
`self.semantic_model(view)` per pull, and the memo's lifetime is exactly that one call — the comment says so ("the lifetime
that makes its memo need no invalidation rule"). The cache this item asks for is therefore a **session-owned model** keyed
`(content_hash, context_hash, env_hash)`, handed to `checks_about` in place of a fresh one, so a debounce after no change
reuses every answer.
**First step landed: the findings are remembered, the model is not.** A pull builds a `SemanticModel` that cannot outlive
the call (it borrows the tree and holds a non-`Send` cache of header views), but its **answers** are plain data. `Session`
now remembers one file's findings under a key that names every input a finding depends on — the summary's key, the closure
fingerprint, the store's revision, the diagnostics configuration, the language and the Unreal switch — and the second pull
of the same state is a lookup. Measured with a bounded probe on `CodeFormat.cpp`:

```text
pull 0   35.9 ms   3 finding(s)
pull 1    2.0 ms   3 finding(s)
pull 2    1.9 ms   …
pull 3    1.9 ms   …
```

`a_pull_is_remembered_until_the_file_changes` pins both halves: the same reading answers the same, and an edit is seen.
`--lib` 738 / 0 / 2, `tests/checks.rs` 108, clippy green. **What this does not fix** is the first pull after an edit — the
typing path — which still pays the inference; that is the next step, and it needs the answers reused **across** readings
(a cache keyed by what a `type_of` question depends on), not a memo of a whole pull.
**Second step landed (`378f11f`): the pump answers the pull before the client asks.** The typing path's cost is the *first*
pull of each new reading — measured on `CodeFormat.cpp`, 36.6 ms after an edit (18.1 ms inference), 109 ms for a session's
first pull, against 2.2 ms for a repeat. The pump has just committed the reading those answers are made from, so
`Session::warm_the_pulls` asks the same question the client will ask (the files a reader has open, at most two) and fills
`checks_memo`; the request that follows is a lookup. Measured:

```text
pull before the edit            38.3 ms
pull again, no change            2.3 ms
(after the edit: warm_the_pulls → 1 answer)
pull after the edit              2.0 ms      ← was 36.6 ms
pull again                       1.9 ms
```

**What is still open in this item** is a *type-level* memo across readings: the 18.1 ms of inference after an edit is
re-derived because `TypeMemo` lives for one request. A content hash cannot key it (an edit changes it), and a range cannot
either (offsets move) — the sound key is the one §2.4 describes, the **definitions the answer actually consulted**, recorded
when it is computed and compared on lookup. Until that exists, the honest statement is: the first answer for a new state
costs what it costs, and this item moves it to the party that is not being waited for.
### 4.3 A commit can hold the writer for 252 ms

Today's `start_shape` reports `worst single commit 252 ms` against the 50 ms `WRITE_BUDGET`. The phases that can do it are
the insert of a large slice and the module units the pump now reads (`889cb32` moved them there). The shape is already
named: a module unit is a **program read** and must use `read_a_program_now`'s three phases (gather under the read lock,
read with every lock dropped, commit under the writer).

* **Acceptance:** worst single commit ≤ 50 ms over a whole pass, with modules queued.

**Settled in `df5bd5f`.** The 252 ms was the probe's own shape: it doubled its wave to 64 while the server caps it
(`INDEX_SLICE`), and the server's cold-start log showed holds of 1–18 ms. With the probe following the server's rule, the
cap turned out to be the wrong instrument — an insert is 1–8 ms for an ordinary header and 13–27 ms for the largest, so 24
files measured 44.3 ms once and 78.4 ms the next time. `commit_a_wave` now stops when it has spent **20 ms**, leaving the
rest of the wave at the front of the queue, already prepared; the wave itself stays large (it sets throughput) and may
double to its cap. Measured: probe worst commit **31 ms** (pass 44.9 s, against 35.0 s unbounded), server cold start
**444 slices, worst hold 33 ms, mean 0.9 ms**. The price is throughput, and it is the next measurement: the cold start of
1 695 files was still indexing after **82 s**.
### 4.4 Phase 4/5 — one cache and consulted-definition invalidation — *after 4.2*

They are what makes 4.2's cache correct under edits (an edit invalidates exactly the readings that read the edited
bodies), so they are not optional, just later. Acceptance as in §2.5/§2.4: one rendering per (file, environment) per
start; a one-line edit invalidates only the refinements that read its bodies.

### 4.5 Phase 3 proper — snapshot publication — *last*

`AnalysisState` is one `RwLock<Option<Session>>` plus a tokio gate, and `Session` carries fifteen `std::sync::Mutex`es
behind it. The `Arc<Published>` of §2.2 is the largest change in this document and, on the numbers above, buys the
difference between a 200 ms pull and a 200 ms pull. Do it **after** 4.2, when the published value is mostly immutable
data; then it is a refactor rather than a speculation.

### 4.6 The other open items, in the review's order

* **Parallel first pass** (cold 94–149 s → 7.1 s here): parsing is a pure function of `(text, config)`; only the inserts
  must be serial. Acceptance: cold pass ≤ 3 s on this project, warm unchanged.
* **Re-baseline every probe input:** a project with non-ASCII text (the panic in `3c2f784` came from one), one whose
  includes resolve, one with modules. Acceptance: five projects in every measurement, no panic.
* **Format versions split by layer**, so a bump degrades a minute instead of an open.

## 5. How each item is verified

```powershell
$env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"

# the pump's shape: open, drain, write holds, first request, steady view
cargo test --release -p cpp_code_analysis --test start_shape -- --ignored --nocapture

# the capability this note says was lost, and the one that must come back
$env:CPPLS_CPP_CORPUS = "C:\Users\zc\Desktop\cpp_project"
cargo test --release -p cpp_code_analysis --test cpp_corpus -- --ignored --nocapture

# the request path, first and steady
node target/hover2.mjs target/release/cpp_ls.exe <project> <file> <needle>

# what a pull is made of, and the same pull with the engine answering Unknown
$env:CPPLS_TIME_CHECKS = "1"; $env:CPPLS_NO_INFERENCE = "1"

# the suite, honestly: a green run with `#[ignore]`s is not green
cargo test -p cpp_code_analysis --lib
cargo test -p cpp_code_analysis --lib -- --ignored
```

A change that does not move its number has not fixed anything, and a test that only passes because it is ignored has not
been fixed either.
