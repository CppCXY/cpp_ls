# Indexing, caching, reuse and locking — a design note

This note is written after a round of measurements on a real project, because the numbers said the current design cannot
be patched into the shape an editor needs. It records what was measured, why the present architecture pays for it, what
the replacement looks like, **what has landed since**, and **what is left** — each remaining item with the number it is
accepted by.

The measurements are from `EmmyLuaCodeStyle` (1243 files scanned, 1761 in the index) on the development machine, with the
server built in release, over the wire, with `target/hover2.mjs` (a faithful client: it answers the server's own
requests) and the pump probes in `crates/*/tests`.

## 0. The baseline, before any of this work

| what | measured | what it said |
| --- | --- | --- |
| cold first pass (per-file summaries) | 94–149 s, ≈54–120 ms/file | single-threaded parse + facts + encode + SQLite |
| warm first pass | 1.9–4.6 s | decode + include verification dominate |
| second pass, before caching | 113–132 s | 13 whole-closure timelines per 16-file drain |
| second pass, after caching | 59.7 s | 1220/1761 served from cache, 81 unit walks |
| steady-state hover | 2–33 ms | the query was not the problem |
| first hover, warm cache | 1.6 s | the request waited before it was answered |
| first hover, cold cache | 37–43 s | …for the whole project's indexing |
| a request that answers before its data is ready | **wrong answers** | bounded waits are not the fix (reverted, `b0dfbb7`) |

## 1. Why the present design pays these prices

1. **The unit of exactness is a translation unit; the unit of caching and of incrementality is a file.** A file's
   meaning can be decided by macro bodies in files it includes, so a per-file summary is a *shape-only* reading until it
   is re-read through its include closure. Repairing one file therefore costs one whole-program parse, and the only key
   that can cache the result describes the whole closure.
2. **A rendering is not a first-class value.** The same file is rendered again for every root that covers it: once as
   the open file's view, once (or more) inside the second pass, once under its own timeline. The caches are split — a
   view cache keyed by text with a handful of entries, a unit cache keyed by closure content, a disk table keyed by
   `(content, env)` with an environment that is not the same number as the summary's. Nothing is shared.
3. **Requests and the pump share one mutable session behind one lock.** A request that finds its file not yet indexed
   takes the write lock and waits for whatever section of the pump is in flight. Measured: the first hover answered in
   1.6 s with a warm cache and 37–43 s cold, and the wait was *upstream of the handler body*.
4. **The startup contract was "the whole project is exact".** Every start re-read the project, because the product
   promised a compiler's view of all of it. No editor can afford that promise; clangd and Rider both keep the index
   approximate and are exact only for what the user is in.
5. **Invalidation is coarse.** A closure fingerprint changes when anything in the closure changes, so one header edit
   discards every refinement whose reading reached it.
6. **The data model mixes environment-free syntax with environment-resolved meaning**, which is what made the repair
   pass necessary in the first place.
7. **A cache-format bump turns every user's next open into a cold start.** `9a414fa` moved `FORMAT_VERSION` to 11; that
   was correct for correctness and expensive for every user.

None of these is a bug in the small. Each is a decision that made one thing easy — per-file incrementality, or a precise
index, or a short write lock — and paid for it with the thing an editor is judged by: time to a correct answer.

## 2. The replacement

### 2.1 Four layers, each with its own key

```text
L1  stub record        per file, environment-free: names, kinds, scope paths, signatures, macro names and
                       spelling-level definitions, include edges. Key: (content_hash, context_hash).
                       Built by a stub parse (no full tree, no macro expansion). This is the cross-file index.

L2  resolved facts     per (file, environment): scopes a macro body opens, declarations inside a macro body,
                       which `#if` branch the program takes. Key: (content_hash, context_hash, env_set).
                       `env_set` is the set of macro definitions actually consulted — see 2.4.

L3  TU view            the rendered text + parse tree for the file a request is about.
                       Key: (content_hash, env_set). One value, shared by hover, completion, diagnostics, inlay.

L4  file text          the VFS. Unchanged.
```

L1 answers the questions an index is for: *where is this name declared, what does its declaration look like, what does
this file include*. L2 and L3 answer the questions a compiler is for, and are built only for a file somebody asked
about. **L1 is never repaired.** That single sentence removes the whole-project second pass.

### 2.2 Answer from a published snapshot, never from a half-built layer

The session's queryable state becomes a value behind an `Arc`, swapped atomically when background work finishes (RCU,
not one `RwLock`). A request:

1. loads the current `Arc<Published>` (an atomic load, no lock);
2. answers from it — always correct **for that snapshot**, never partial;
3. if the answer needs L2/L3 for the file and the snapshot does not have it, the request **starts that work** and
   answers from L1 (an approximate but honest answer), or reports no answer for this position.

The lesson of the reverted commit is the third step: a request may not answer from a *partially applied* layer, and it
may not silently answer `null` while a better answer is one keystroke away. "Answer from the last published snapshot"
gives correctness; "start the work" gives freshness. Background refinement may take as long as it likes, because it holds
no lock a request needs.

### 2.3 Resolve a file by composition, not by parsing its closure

The reason a file's meaning needs a closure is the macro bodies in force. But a body is needed only for the macro names
a file actually invokes, and its shape is what matters:

```text
invocation name  →  index lookup: the definer's stub record  →  the body's text
                 →  the body's shape (BodyShape: namespace / close / block / qualifying / other)
                 →  splice that effect into the file's own parse
                 →  if the shape is `Other` and the body begins with a name, resolve that name the same way
                    (bounded depth, cycles are `unknown` — never a guess)
```

This is a lazy `MacroBodies` resolver over L1, and the parser already takes one (`with_macro_bodies`,
`shape_of_a_body_at`). It turns "read this file in its program" into "read this file, with a handful of lookups".

### 2.4 Invalidation by consulted definitions

An L2/L3 entry records what it depended on:

```text
definitions: [(definer_path, body_range, body_hash), …]      // the bodies it actually read
includes:    [(spelling, resolved_path), …]                   // the searches it actually made
```

An edit to file `F` invalidates exactly the entries whose own content is `F`, whose `definitions` name `F`, or whose
`includes` resolved through `F`. It does not invalidate "everything that reaches `F`", which is what a closure hash does
and why one header edit costs the project.

### 2.5 One cache, byte-bounded

Replace the three render caches with one table (memory LRU by bytes, backed by the existing disk database):

```text
key:   (content_hash, context_hash, env_hash, kind)     kind ∈ {stub, resolved facts, rendering, tree}
value: encoded record
```

Every consumer reads it; every producer writes it once. The environment hash is a hash of the `env_set` above.

### 2.6 Startup contract

What `initialize` promises: **an answer to any request about any project file, correct for a published snapshot**, from
L1, as soon as project files have stub records. Refinement runs afterwards, in this order: the open files, then their
closures, then the rest. It never blocks a request and never changes a published snapshot under a reader's feet.

## 3. Where the work stands

| phase | state | measured after |
| --- | --- | --- |
| 0 — lazy refinement | **landed** `7bc4c60` | start **2.26 s** (was 62 s: first pass 1.9 s + second pass 59.7 s), **0** timelines, 1761 files left alone; one requested file refined in **93 ms** with one timeline |
| 1 — answer from L1, ask for L2 | **landed** `bb7bce2` | a definition answered in **26.5 ms** from the index; the declaring file refined **118 ms** later; `is_refined` false → true |
| 2 — composition instead of a closure parse | **landed** `b010584` | one file refined in **25.8 ms**, **0** timelines (was 118–191 ms, one); the session builds no timelines for the pass at all |
| 3 — snapshot publication | **partial** `889cb32`, `b9d83cf`, `da3c35c`, `334f7ed` | first hover **333 ms** (was 1 850 ms), steady **2 ms**; warm `Session::open` **99 ms** (was 141 ms) |
| 4 — one cache, parallel index | not started | — |
| 5 — invalidation by consulted definitions | not started | — |

Two measurements from phase 3's investigation are worth keeping because they were surprising:

* the first hover's 1.5 s was **`read_the_modules`**, which drained the whole index queue
  (`while session.pending() > 0 { session.advance(256) }`) inside the request's write lock, for a file with no `import`
  (`889cb32` moved the draining to the pump: **1 850 → 439 ms**);
* after that, the remaining **~334 ms** was **before the handler ran** — the handshake (`initialized` 126 ms, including
  `Session::open`) and the dispatch of the notifications sent ahead of the request.

## 4. Remaining work

Each item is one that can be landed and measured on its own, in the order they are worth doing.

### 4.1 The message loop must not wait for the session to open — *0.5 day*

`initialized` awaits `open_session` (99–141 ms) and `register_files_watch`; a `didOpen` and a hover sent right after
`initialized` therefore wait for the whole handshake. That is the remaining ~334 ms of the first request.

* **Change:** start the session open as a task, and let requests that arrive before it exists answer "no analysis" (the
  state every handler already handles); publish the session when it is ready.
* **Acceptance:** first request ≤ 50 ms whether or not the session is open; the probe's first-answer-with-content is
  ≤ 150 ms on a warm cache.
* **Risk:** a hover in the first ~100 ms answers `null`. Acceptable for one request, and measurable in the probe.

### 4.2 The project scan inside `open` — *0.5 day*

`Session::open` is 99 ms warm and the compiler is no longer part of it (`334f7ed` cached the discovery: 141 → 99 ms), so
what is left is the scan of 1243 files and the session's setup. Measure the split first (`project::discover` vs the file
walk), then cache the directory listing by mtime, or parallelise it.

* **Acceptance:** warm `Session::open` ≤ 20 ms on this project.
* **Risk:** a stale scan serves a file list that misses a new file; the watcher's events and a cheap directory mtime check
  are the guard.

### 4.3 Snapshot publication, proper — *3–5 days*

`AnalysisState` is one `std::sync::RwLock<Option<Session>>`; requests and the pump share it. Phase 3's remaining work is
the `Arc<Published>` of 2.2. `Session` is not `Clone`, so the published value has to own the query-relevant data:

1. move `ProjectIndex` and the view/rendering caches into `Published`; a file's record is an `Arc`, so a slice's inserts
   clone the outer map only (`Arc::make_mut`);
2. `with_snapshot`/`run_blocking` become `published.load()` — no `std::sync::RwLock` on the request path at all;
3. notifications write buffers (`OpenDocuments`, already lock-free) and queue; they take no write lock.

* **Acceptance:** no `std::sync::RwLock` read on the request path; first request ≤ 50 ms cold; steady hover ≤ 10 ms;
  `WRITE_BUDGET` still enforced on the publish step.
* **Risk:** the largest change in this document. Mitigation: the index is already the piece requests ask most; publish it
  first and leave the rest behind the old lock, then move the caches one at a time.

### 4.4 Module units are read where a program read happens — *1 day*

`889cb32` moved the module draining to the pump, but the pump reads up to **four module units per slice inside the write
lock**, and a module unit is a **program read** (~500 ms). One `import std;` project will hold the write lock for
seconds on that slice.

* **Change:** take the three phases `read_a_program_now` already uses — gather under the read lock, read with **every lock
  dropped**, commit under the writer — and use them for module units.
* **Acceptance:** a slice's write hold stays inside `WRITE_BUDGET` (50 ms) with modules queued.

### 4.5 One rendering per (file, environment) — *2 days*

Phase 4's first half. Today a file can be rendered for the open view, for the pass, and for a hover, in three structures.
Key them all by `(content_hash, context_hash, env_hash)` in one byte-bounded cache (2.5).

* **Acceptance:** one render per file per environment per start (instrument the count and assert it); hover of a
  declaration in a file that is not open ≤ 50 ms warm.
* **Risk:** memory. The LRU is by bytes, and the cache sizes are already reported at drain time.

### 4.6 Invalidation by consulted definitions — *2 days*

Phase 5. Replace `the_closure_fingerprint` (coarse: any change in the closure invalidates the refinement) with the set of
definitions the reading actually consulted (2.4). The pass already knows those definitions: they are the entries of
`the_indexed_environment`, and the ones it looked up while deciding shapes.

* **Acceptance:** a one-line edit in one header invalidates only the refinements that read its bodies — count the
  invalidated entries before and after the change on this project.
* **Risk:** the consulted set must be recorded at build time and compared at lookup; missing one entry means a stale
  reading, which is why the closure fingerprint stays as the outer guard until this is measured.

### 4.7 Parallel first pass — *2 days*

The cold first pass is 94–149 s: 54–120 ms per file, single-threaded. Parsing is a pure function of `(text, config)`;
only the index inserts must be serialised.

* **Change:** a worker pool over the queue's files, each producing a `Prepared`, with the existing single writer committing
  in batches.
* **Acceptance:** cold first pass ≤ 30 s on this project (8 cores), with the warm numbers unchanged.
* **Risk:** the store's HTTP-free caches (`directives`, `resolution`) are behind mutexes already; the wave's
  `prepare_closure` already runs two files at a time (`parallel_map`), so the shape exists.

### 4.8 The ten tests that encode the eager contract — *1 day*

They assert that a start (or one drain) refines every file, which phase 0 removed on purpose. Each must be rewritten to
the new contract — a start refines nothing, a request refines its target, the pump drains what was asked:

```text
a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session
a_configuration_change_re_arms_a_worked_file_and_it_is_rebuilt
a_file_read_as_a_program_needs_no_guard_filter
a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass
a_keystroke_in_a_body_reads_the_file_through_its_timeline
a_program_too_large_to_read_at_once_is_left_alone
a_refinement_is_served_to_the_next_session
a_rendering_is_not_left_in_the_cache_for_the_next_session
a_unit_survives_typing_in_a_body_and_not_a_change_to_a_directive
a_wave_with_nothing_to_parse_still_carries_the_drains_timelines
```

Two of them (`a_rendering_is_not_left_in_the_cache_for_the_next_session`, `a_refinement_is_served_to_the_next_session`)
are the **correctness** guards of the refinement cache and must keep passing after the rewrite — they are the reason
`env_hash` exists in the key.

### 4.9 Format versions — *0.5 day*

`FORMAT_VERSION` moved for a cache-semantics change (`9a414fa`), which is correct but makes every user's next open cold.
Split the versions by layer (L1 stub records, L2 refined facts) and add a background warm-up so a bump degrades the first
minute rather than the first open.

## 5. How each item is verified

```powershell
# request latency (first and steady, cold and warm)
node target/hover2.mjs target/release/cpp_ls.exe <project> <file> <needle>

# what a start and one requested file cost
$env:CPPLS_REAL_PROJECT = "<project>"
cargo test --release -p cpp_code_analysis --test pump_probe -- --ignored --nocapture   # pump counters
cargo test --release -p cpp_code_analysis --test tmp_open_probe -- --ignored --nocapture # (temporary) open time

# the second pass's slices and what it decided
$env:CPPLS_TRACE_COOKING = "1"
```

The probes are the point: every item above names a number measured on `EmmyLuaCodeStyle` with these commands, and a
change that does not move its number has not fixed anything.
