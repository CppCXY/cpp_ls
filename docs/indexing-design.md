# Indexing, caching, reuse and locking — a design note

This note is written after a round of measurements on a real project, because the numbers said the current design cannot
be patched into the shape an editor needs. It records what was measured, why the present architecture pays for it, and
what the replacement looks like. It is a design, not a decision log: the phases at the end are each shippable.

The measurements are from `EmmyLuaCodeStyle` (1243 files scanned, 1761 in the index) on the development machine, with the
server built in release, over the wire, with `target/hover_time.mjs` and the pump probes in `crates/*/tests`.

| what | measured | what it says |
| --- | --- | --- |
| cold first pass (per-file summaries) | 94–149 s, ≈54–120 ms/file | single-threaded parse + facts + encode + SQLite |
| warm first pass | 1.9–4.6 s | decode + include verification dominate |
| second pass, before caching | 113–132 s | 13 whole-closure timelines per 16-file drain |
| second pass, after caching | 59.7 s | 1220/1761 served from cache, 81 unit walks |
| steady-state hover | 2–33 ms | the query is not the problem |
| first hover, warm cache | 1.6 s | the request waits before it is answered |
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
   takes the write lock and waits for whatever section of the pump is in flight. Measured: the first hover answers in
   1.6 s with a warm cache and 37–43 s cold, and the wait is *upstream of the handler body*.
4. **The startup contract is "the whole project is exact".** Every start re-reads the project, because the product
   promised a compiler's view of all of it. No editor can afford that promise; clangd and Rider both keep the index
   approximate and are exact only for what the user is in.
5. **Invalidation is coarse.** A closure fingerprint changes when anything in the closure changes, so one header edit
   discards every refinement whose reading reached it.
6. **The data model mixes environment-free syntax with environment-resolved meaning**, which is what makes the repair
   pass necessary in the first place.
7. **A cache-format bump turns every user's next open into a cold start.** The refinement cache written in `9a414fa`
   moved `FORMAT_VERSION` to 11; that is correct for correctness and expensive for every user.

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

The session's data becomes an immutable value behind an `Arc`, swapped atomically when background work finishes
(RCU, not one `RwLock`). A request:

1. loads the current `Arc<SessionData>` (an atomic load, no lock);
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
`shape_of_a_body_at`). It turns "read this file in its program" into "read this file, with a handful of lookups", which
is the difference between 0.5–2 s and a few milliseconds. `the_settled_shape_with`, prototyped this round and abandoned,
is the right idea; the piece that was missing is that the resolver must come from the **stub records**, not from a
parsed closure.

### 2.4 Invalidation by consulted definitions

An L2/L3 entry records what it depended on:

```text
definitions: [(definer_path, body_range, body_hash), …]      // the bodies it actually read
includes:    [(spelling, resolved_path), …]                   // the searches it actually made
```

An edit to file `F` invalidates exactly the entries whose own content is `F`, whose `definitions` name `F`, or whose
`includes` resolved through `F` (plus `#include` edges, which is a separate, already-supported case). It does not
invalidate "everything that reaches `F`", which is what a closure hash does and why one header edit costs the project.

### 2.5 One cache, byte-bounded

Replace the three render caches with one table (memory LRU by bytes, backed by the existing disk database):

```text
key:   (content_hash, context_hash, env_hash, kind)     kind ∈ {stub, resolved facts, rendering, tree}
value: encoded record
```

Every consumer reads it; every producer writes it once. The environment hash is a hash of the `env_set` above.

### 2.6 Startup contract

What `initialize` promises: **an answer to any request about any project file, correct for a published snapshot**, from
L1, as soon as project files have stub records. In practice that is the scan plus one stub parse per file, which is the
part that can be parallelised (parse is a pure function of text and configuration).

Refinement runs afterwards, in this order: the open files, then their closures, then the rest by size. It never blocks a
request and never changes a published snapshot under a reader's feet.

## 3. Phases, each with an acceptance number

| phase | change | acceptance |
| --- | --- | --- |
| 0 | Lazy refinement: no second pass at startup; refine on open/request; keep the `9a414fa` cache | warm start = first pass only (≤ 5 s); hover on a fully read file 2–33 ms |
| 1 | Stub layer (L1) from the existing summary, answered first; `isIncomplete` only when the answer needs L2 | cold start answers hovers from L1 within ~1 s of the scan |
| 2 | Composition resolver (2.3) + `env_set` keys (2.4) | first refinement of an open file ≤ 100 ms; no closure parse in the request path |
| 3 | Snapshot publication (2.2) | first request latency ≤ 50 ms cold, hard bound, never waiting for the pump |
| 4 | One cache (2.5) + parallel stub index | warm start ≤ 2 s; rendering of one file computed once |
| 5 | Invalidation by consulted definitions (already exercised by 2.4) | editing one header invalidates only the entries that read its bodies |

Phase 0 is small and can land first. Phase 3 is the largest rewrite and the one that removes the last shared lock;
phases 1–2 are what make phase 3's snapshot cheap to publish.

## 4. What this round's work is worth keeping

- `6ba6a14` — the three-way plain shape (`Used` / `Settled` / `Unplaceable`): the classification phase 2 needs.
- `9a414fa` — refined readings stored under an environment key, the collection fix, and the flush: the caching
  machinery, with `env_set` replacing the closure fingerprint in phase 2.
- The probes: `target/hover_time.mjs` (request latency and content latency, cold and warm) and the pump probes. Every
  phase above is measured with them; the numbers in the table at the top are the baseline.
- The reverted experiment `b0dfbb7` is the reason 2.2 insists on answering from a **published** snapshot: a request that
  answers before its data is ready is not fast, it is wrong.
