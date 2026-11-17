# The module cache: an environment is a value, and a parse is a function of it

Status: design, not implemented. Everything marked **measured** was measured on `EmmyLuaCodeStyle` or `test_cpp`; the
rest is a claim with the test that would settle it.

This is the change `docs/memory-status.md` calls "the only one left that moves the *shape* of the number". That page
establishes that the retained bytes (summaries 113 MB) are no longer the problem; this one is about the two numbers
that are: **what one query pays for** (2.8–8.8 s and +174–355 MB per whole-program read) and **what the session keeps**
(the union of every closure it ever walked).

## 1. The claim

A file's reading is **not** a function of its translation unit. It is a function of three things:

```text
text of the file          its own bytes
context                   configuration + the file's own directory (quoted includes)
environment               the macros in force at the point it is included
```

The translation unit is how a compiler *arrives* at the environment; it is not the environment. Today this analysis
confuses the two: to read one file it walks the whole closure (`TranslationUnit::walk`, one text scan per file in the
closure — **measured**: `LuaCheck.cpp` reaches 373 files, 844 940 tokens, 8.8 s), and it keys the result by the
closure (`SummaryStore::the_closure_fingerprint`: a hash of every file's path and content in the closure).

The change is to make the middle value first-class:

```text
Env    a persistent map  name -> definition,  with a Merkle hash (EnvId)
Site   (file content, context, incoming EnvId)          -- an inclusion point, memoised
Parse  (file content, context, reachable EnvId) -> facts -- the cache entry
```

Then reading a file costs **the include spine**, not the closure: a fold over per-file *directive skeletons* (already
in `FileSummary`), each memoised by `Site`. Nothing re-scans a file's text to answer a question about another file.

## 2. Multiplicity: the same file, many environments

The question "can one file have several environments?" has three separate answers, and the design has to get all
three right.

### 2.1 Across translation units — yes, and that is C++

`a.cpp` and `b.cpp` can include `api.h` with different macros in force (`-D`, a `#define` before the include, a
`#if` around it). Then `api.h` has two readings, and a cache keyed by the file alone would answer one for the other.

**Measured** (`docs/open-questions.md`, the "135 contexts" correction): on `EmmyLuaCodeStyle`'s closure, 1 761 files,
1 723 distinct content hashes, 38 files stored twice, and **1 distinct macro environment over the whole closure**. A
kernel- or Chromium-shaped project has several environments (one per configuration variant); the number of *entries*
is `distinct (content, reachable env)` pairs, not `files × roots`.

### 2.2 Within one translation unit, at several inclusion points — yes, and that is what guards are

`bits/version.h` in libstdc++ is deliberately **unguarded**: every header includes it to add the feature-test macros
*that header* asks for. So the same file is entered several times in one program, and its environment differs each
time. `TranslationUnit::walk` already knows this (`walk_one_file`'s doc explains why a second read is not skipped for
an unguarded file). The key must therefore be the **site**, not the file:

```text
SiteId = (content_hash, context_hash, incoming EnvId)
```

The incoming `EnvId` captures exactly the state at the `#include`, so two sites with the same state share one entry,
and `from_offset` is not in the key (it is in the diagnostic data, not in the answer).

### 2.3 Within one translation unit, differing in macros the file never mentions — no, and that is the sharing

An environment is a *reachable* value from the file's point of view. If 1 000 macros differ between two programs and
the file mentions none of them, its reading is the same, and the key must say so or the cache degenerates into
"every header once per program". This is what `MacroView::environment_fingerprint` already computes (see §4).

**So the answer to "does the same file get several environments?" is: only as many as the distinct *reachable*
environments that reach it.** The raw reading (§5) is shared across all of them; only the rendered form is per
environment.

## 3. The model

### 3.1 Env: a persistent map with a hash

```rust
struct Env {                      // persistent, structurally shared
    parent: Option<EnvRef>,       // O(1) snapshot of an earlier state
    overrides: SmallMap<NameId, Option<MacroId>>,  // this step's #define/#undef
    id: EnvId,                    // Merkle hash of (parent.id, sorted overrides)
}
type EnvId = u64;
```

`define`/`undef` copy a path of nodes (a handful) and update the hash on the way out: O(log₃₂ N) with O(1) hash work,
no table copy. Two states are equal iff their ids are, so **key comparison never walks a table**.

`MacroId` is an interned definition: `(NameId, function_like, parameter_text_hash, body_text_hash, body_tokens_hash)`.
The body hash must cover what expansion reads — the replacement list as tokens, plus the parameter list — and nothing
else (whitespace and the definition site are diagnostics, not meaning).

### 3.2 Site: the memo that replaces the walk

```text
compose(content, context, incoming: EnvId) -> SiteResult {
    env_at: Vec<(offset, EnvId)>,   // the state after each #include, for diagnostics and for nested includes
    final:  EnvId,                  // the state at the end of the file
}
```

`compose` is a **fold over the file's directive skeleton**, not over its text:

```text
for each directive in order (from FileSummary.macros / .includes, ordered by range):
    #define/#undef  -> env = env.define(name, id) / env.undef(name)
    #if/#elif/#else -> evaluate the stored region question against env
                       (the branch is already flattened: a fact's guard names its region)
    #include        -> site = SiteId(child_content, child_context, env.id)
                       child  = compose(child_content, child_context, env.id)   // memoised
                       env    = child.final
```

Everything it reads is already stored per file: `MacroFact` (name, kind, function like, body, `value`, `alias`,
`range`, `guard`), `IncludeFact` (spelling, resolved, `is_next`, `guard`), and `SummaryGuards` (the regions and their
questions — "a region is stored as its **question**, never as its answer", which is exactly what an environment is
for).

**Cost:** one memo hit = one hash lookup, then the answer (`final EnvId`) is in hand. A miss = the file's directives
(typically 20–400) × (a name interning lookup + a condition evaluation over a few tokens) + one memo lookup per
include. There is no lexing of the file and no parse of the file.

### 3.3 Keys

```text
RawKey     (content_hash, context_hash)                          -- the file's own syntactic reading
ParseKey   (content_hash, context_hash, reachable EnvId)         -- the rendered reading, when one is asked for
SiteKey    (content_hash, context_hash, incoming EnvId)          -- the compose memo
```

`RawKey` is today's `SummaryKey` **without `env_hash`**. That is deliberate and it is the largest single saving: the
raw reading stores every fact *with the region it was written in* (`FactGuard::Region`), so it does not depend on the
environment at all — a guarded `#define` is recorded in both branches, each tagged. One entry per file, shared by
every program that reads it. **Measured**: `EmmyLuaCodeStyle`'s summaries are 1681 entries / 43.4 MB, and the cooked
readings are 388 / 13.2 MB — today both are already close to per-file, but the *closure key* on the cooked side
(section 4.3) is what must change.

## 4. The reachable environment: what is hashed, and how fast

### 4.1 What must be in it

The mention set of a file is a pure function of its text, and is computed once per content hash:

* every **identifier token** of the file (it may be a macro call);
* every name in a **condition** (`#if A && B`, `#ifdef C`, `defined(D)`), including inside `#elif`;
* the **argument of an `#include`** when it is a macro (`#include HEADER`);
* every name **mentioned by the body of a macro** in the set, transitively — expanding `A` looks up `B`;
* **not** the seed (`-D`, the compiler's predefines): that is `context_hash`'s half (see
  `environment_fingerprint`'s doc, which states this).

### 4.2 The fast path

```text
reachable(content, incoming: EnvId) -> EnvId
    mentions = mentions_of(content)                 // memoised by content hash: Vec<NameId>, sorted, deduped
    pairs    = mentions.iter().map(|n| (n, env.lookup(n)))   // O(k) hash lookups, u64 ids only
    hash     = fnv1a64(pairs as little-endian u64s)          // no strings, no allocation
```

with a second memo `(incoming EnvId, mentions_hash) -> reachable EnvId` for the hot path (a hover on the same file
while the environment is unchanged). Everything is `u64`:

| step | today | this design |
|---|---|---|
| whole-table hash (`environment_hash`) | O(M log M) with a `String` per name and per body; M = every macro in force (10⁴–5·10⁴) — measured at **~1–3 ms** and ~1 MB of transient per key | not on any query path |
| reachable hash | `environment_fingerprint` exists but is not the key: O(k) with `&str` keys and a sort, k = mentions (10²–5·10³) | **~5–40 µs**, u64 only |
| memo hit | — | **~50 ns** (one hash lookup) |
| compose memo hit | — | **~50 ns** per include edge on the spine |

The point of the interning is that **no key is ever built from text**. A `MacroId` is computed once, when the
definition is first indexed (the file is being read anyway); from then on a definition is seven numbers.

### 4.3 Where today's cost actually is

Three different "environments" exist in the code, and only one of them is a key:

1. `SummaryStore::environment_hash(facts)` (store.rs) — the whole macro table, sorted, as bytes. Used when a summary
   is written from a rendering. **Measured** as the ~1–3 ms above; it is also what destroys sharing when two programs
   differ in a macro nobody's file mentions.
2. `SummaryStore::the_closure_fingerprint(path)` (store.rs) — path + content over the whole closure, sorted FNV.
   This is the cook-record key, and it is why a per-file cook record is still per program: two programs with the same
   header get two records. Replacing it with `reachable EnvId` is what makes the record shared. (**Measured** on
   `test_cpp`: 106 stored cooks served in one warm run; on a project with any environment variation the same header
   would be re-cooked once per variant.)
3. `MacroView::environment_fingerprint(source, tokens)` (summary.rs) — the right function, wired to nothing that is a
   key yet.

## 5. Sharing, layer by layer

```text
raw reading       one per (content, context)                1681 entries, 43.4 MB measured
rendered reading  one per (content, context, reachable env) 388 entries, 13.2 MB measured
parse tree (view) one per rendered reading, LRU              bounded (VIEW_BYTES)
```

A header included by 40 programs under 40 environments costs **one** raw reading and 40 rendered readings *only if
someone asks for a rendered form in all 40*. Queries that work from facts (definition, references, hover of a type,
completion of a qualified name) ask for the raw reading and the environment at the site — no render at all.

## 6. Soundness: what the key must include, and why

| the reading depends on | where it is in the key | the test that would catch a mistake |
|---|---|---|
| the file's bytes | `content_hash` | `a_stored_cook_answers_the_next_session` (exists) |
| the directory (quoted `#include`) and the configuration (`-I`, `-D`, `-std`) | `context_hash` | `resolution_still_holds` (exists, re-resolves every include on a hit) |
| the state at the include point | `reachable EnvId` / `incoming EnvId` | a differential test against `TranslationUnit::walk` on a corpus (§7, gate 1) |
| `__LINE__`, `__FILE__` | the site: if the file mentions them, add `from_offset` (and the includer's path) to the key | a fixture that includes one header from two offsets and reads a `__LINE__` value |
| `__COUNTER__` | not cacheable per site; mark the file, recompute | a fixture with two includes of a `__COUNTER__` user |

**The differential test is the gate.** `compose` must produce, on a corpus of real headers, the same *answers* as
`TranslationUnit::walk`: for every file in a unit, and a fixed list of probes (is `X` defined, what is its body, is
this region in force), the composed environment must agree with the walked one. That is a stronger and cheaper test
than checking that the two produce the same `EnvId`.

Two known deviations that must stay deviations, and are documented where they are implemented:

* an include that does **not resolve** (`has_unresolved_includes`) is not a candidate for the raw cache — a summary
  that records a failed search must not be stored, because nothing in the key would notice the header appearing;
* a summary built **from a rendering** is not stored under the raw key, for the reason store.rs already gives.

## 7. Staged plan, with the number that decides each stage

**Gate 0 (a day).** A differential probe: for 20 files of `EmmyLuaCodeStyle`, `compose` vs `walk`, with 50 probes per
file. It runs before any production change; if it disagrees, the design is wrong and nothing else matters.

**S1 — delete the walk from the cook (2–3 days).** `cooking_materials` keeps only the composed arm; `compose` above;
`the_closure_fingerprint` replaced by `reachable EnvId` for the cook record. Gates: Gate 0 green; the C++ corpus stays
51/52; the two directive-split tests pass; single cook on `LuaCheck.cpp` from ~8.8 s to **< 100 ms**; 82 open files in
one `advance(64)` from **2.87 GB to < 500 MB**.

**S2 — the raw cache without `env_hash` (2 days).** `SummaryKey::env_hash` becomes 0 for raw readings; the reachable
env moves to the cooked key only. Gates: on Emmy, the same header read by two `.cpp` files with different macros
serves **one raw entry**; warm start reads 1046 → **0** files (see `memory-status.md` §3).

**S3 — bounded working set (3–4 days).** `Arc<FileSummary>` + LRU + reload from redb; `Arc<str>` for the VFS with open
buffers pinned; views under `VIEW_BYTES`. Gate: Emmy steady-state tables ≤ **256 MB**, private peak ≤ **400 MB**, and
a 10× larger synthetic project leaves both unchanged.

**S4 — the project index as the product (1 week).** The batch path (`inspect`, the census probes) writes the raw
readings and the interning tables; the server loads the name/file tables only and composes on demand. Gate: open a
file in a 100 000-file synthetic tree at the same latency as in Emmy.

## 8. Open questions

* **Condition evaluation in `compose`.** `SummaryGuards` stores the condition *spans*; evaluating them needs those
  few bytes of text. Either the skeleton stores the condition text inline (a few hundred bytes per file) or `compose`
  reads the region from the VFS. The first is bounded and testable; the second keeps summaries smaller. Decide with a
  measurement of the two cache sizes on Emmy.
* **`#include` inside a skipped branch.** The fold must evaluate the guard *before* recursing; a naive fold that
  recurses first would compose environments for headers a compiler never reads — wrong answers on `#if 0` blocks.
* **`#include_next` and the search order.** `IncludeFact::resolved` is re-verified today; a composed environment must
  not become a second, unchecked resolution. Keep the re-verification on the hit path.
* **Transitive include closure vs. direct.** `env_at` after an include is the child's *final* environment, which is
  the child's whole subtree. That is C++ semantics (an include splices its whole text), and it is why the closure is
  a real dependency — the point is that it is a *value* computed by folding memoised deltas, not a *scan*.
* **Where the memo lives.** Per session is simplest; a process-wide (static) memo would share across workspaces but
  must be bounded and cleared by generation. Start per session, measure the hit rate, then decide.

## 9. What this does not fix

Template instantiation, overload resolution and ODR checks stay translation-unit-wide. No C++ front end makes those
incremental, and clangd does not either. They belong to the batch path (S4) and to lazy instantiation on query — a
separate design, out of scope here.
