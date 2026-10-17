# Why `walk` needs a redesign, and what the redesign is

## 1. What it costs, measured

```text
EmmyLuaCodeStyle, MSVC toolchain, one real entry file (before the instrumentation was removed)
  wall 112.5 s          instrumented work 233.1 s   (the counters summed across threads; the two are not comparable)
  [indexing]  80.8 s   34.6%     scanning, reading, parsing, summaries
  [units]    152.4 s   65.4%     building translation units        <- the problem
    walk-macro-guard  61.2 s  26.2%   one stage inside it
  [detail]   107.2 s             inside the stages above
```

And from the census run (same project, before it was removed):

```text
1388 roots   ->   83,697 file-visits   ->   10,792,034 conditional evaluations
83,697 / 1,388 indexed files  ~=  60      the same header is entered about sixty times
```

**The number that matters is the 60.** Every other figure is that one, multiplied.

## 2. Why it is like that — the concept, not the code

A C++ preprocessor's entire state at any point is *(which file, which names are in force)*. Two translation units that
include the same header with the same names in force see **the same program** at that point: the same regions live, the
same definitions added, the same includes taken.

The present design builds a translation unit **per root**, eagerly, re-deriving macro state, conditions and definitions
from scratch, and throws all of it away when the next root starts. Only the root's own text differs, and the header part
— where the vast majority of the work is — is recomputed every time.

That is why every profile of this walk ends up pointing at "guard evaluation". Evaluating one condition is not
expensive. Evaluating the *same* condition with the *same* names in force sixty times is.

`58a7e4f share a timeline between roots whose closures are the same` was a step in the right direction and it is worth
keeping — but its key is "the closure is the same", and closures differ between roots the moment any include differs.
The sharing has to be at a finer granularity than the root.

## 3. What the servers that are fast actually do

```text
clangd      the preamble (the leading #include run) is compiled once into a PCH and reused; invalidated by the
            preamble's bytes, the compile command, or the on-disk state of any file it included. Small macro
            changes travel as a *patch* rather than a rebuild.
CLion / VS  header ASTs are indexed once; per translation unit only the macro state is replayed.
```

The shared concept is not "cache the root". It is:

> **`(file, environment) -> what that file contributes` is the thing worth memoizing, and it is a pure function.**

Everything below is that idea expressed in this codebase's own data structures.

## 4. The redesign

### 4.1 The environment is a *set of names*, and that makes it cheap

The walk carries **definedness and not values** (`UnitState`'s own note says so). So the environment a condition is
answered against *is* the set of names in force — nothing else. Two consequences:

* a digest of that set is a **complete** identity, not an approximation: equal digests ⇒ every condition answered
  identically;
* it can be maintained as the set changes with **one hash per `#define`/`#undef`** — XOR of per-name hashes, which is
  exact for a set (adding and removing cancel; redefining a name already in force is a no-op) — and read **once per
  file entered**, never per condition.

This is the part the first attempt got wrong, and it is worth writing down why: that attempt keyed a cache by
`(file, offset, digest)` **per condition**, allocated a `PathBuf` per lookup, and maintained the digest per definition
— so it paid a constant cost on 10.8M lookups and changed the key on every `#define`. Measured: 81 s → 138 s. The
granularity was the error, not the idea.

### 4.2 What is cached, and why it can be spliced

A file's contribution is its **frame subtree**: the events and frames it produced, its include edges already resolved,
and which of its regions were live. The timeline is a flat `Vec<TuFrame>` where each frame carries `parent` and
`from_in_parent` — so a subtree is **relocatable by construction**: re-parenting is an index rewrite plus an offset
adjustment, not a re-walk.

### 4.3 The walk becomes a memoized DAG traversal

```text
walk(root):
    for each include edge (file, digest_after_the_edge):
        cache hit  -> splice the subtree under the current frame
        cache miss -> walk it, store the subtree, splice it
    # only the root's own text is ever walked fresh
```

Total work becomes **O(distinct (file, environment) pairs)** instead of O(roots × closure). The measured 60×
duplication is the headroom: if even a third of it is removable, the `[units]` stage goes from 152 s to tens of
seconds, and the same structure makes the *first* pass and every later edit cheaper.

### 4.4 Persistence, and invalidation

The unit cache already writes units through a codec (`.cppls`). The same codec stores these contributions, so a
restart starts warm — that is the PCH-on-disk half of clangd's design, in our own format.

Invalidation needs no new machinery: a changed file drops its entries through the existing per-path `forget`, and a
changed *environment* is a different key by construction. (clangd folds "the on-disk state of an included file" into
its compatibility check; we already have that as `forget(path)`.)

## 5. What must change in the code

```text
TimelineBuilder        open a frame whose subtree comes from the cache (the splice)
TranslationUnit::walk  key by (file, digest); consult the cache per include edge; store what it walked
the unit cache's key   (path, digest) rather than (path, context) — or in addition to it
UnitState              maintain the name-set digest (one hash per definition change)
```

## 6. What *not* to do

* **Do not parallelise the walks first.** It multiplies the same waste by the core count and hides the design problem
  behind a smaller wall-clock number. (It is a fine *last* step, after the work is worth doing at all.)
* **Do not micro-optimise the guard evaluator.** Six microseconds per evaluation is not the disease; deciding the same
  thing sixty times is.
* **Do not measure with timers inside the library.** See §7.

## 7. Measurement, after the instrumentation was removed

The `StageTimer` call sites (66 of them, several per file and per condition) and the `query_trace` regions are gone,
along with the two probes that existed only to read them. They were removed because **a timer on a path taken once per
condition changes the thing it measures** — and because the numbers they produced had already been misread once
(`Σstages` accumulates across threads and is not a wall clock).

The rule that replaces them:

> **The library carries no instrumentation. Probes time whole operations from the outside, with `Instant` around the
> call.** Two `Instant::now()` calls per *query* cost nothing and measure the figure a user actually feels.

If a future investigation genuinely needs interior numbers, it goes in a probe that **proves it ran** — a line printed
whenever it is switched on, so that "it did not run" can never be mistaken for "it measured zero". That mistake was
made three times in one session; the rule is cheap and it is the reason this document has numbers in it at all.

## 8. Migration, each step measurable from the outside

```text
1. the digest, and a (file, digest) -> contribution cache that only *counts* hits and misses
   a probe reports the hit rate on EmmyLuaCodeStyle. If it is not high, stop and reconsider — the rest is
   only worth building if the duplication is really there.
2. splicing: TimelineBuilder takes a cached subtree
3. the walk consults the cache; compare wall-clock load time on the same project and the same entry file
4. persistence through the existing codec
```

Step 1 is a day's work and it decides whether steps 2-4 are worth doing. That is the point of doing it first.
## 9. The first slice was built, measured, and **falsified** its own assumption

§4.1 claimed that a header is "entered with the same names in force about sixty times". That is **wrong**, and the
measurement says so:

```text
implemented   the environment digest (name + where the definition was written, maintained as the set changes)
              and a `(file, digest) -> the file's conditional answers` cache, at *file* granularity — one lookup
              per entry, no per-condition key, no allocation on the miss path
measured      EmmyLuaCodeStyle, 1122 files, one real entry file, timed from outside:
                without the cache   87.4 s / 95.6 s  (two runs)
                with the cache     104.2 s
reverted      six blocks, `cargo check` clean
```

The 60× is the number of **visits**, not of **distinct environments**. Every root defines its own macros — and its
own headers do — *before* it reaches a shared header, so the set in force at that header's entry differs from root to
root, and the digest almost never repeats. The cache paid a lookup per visit and bought nothing.

### What this points at instead

The assumption was one step too coarse. A file's conditions do not depend on the *whole* set in force — only on the
**names those conditions mention**:

```text
#if defined(_STD_BEGIN) || _HAS_CXX20     depends on two names, and on nothing the root did
```

And which names a file's conditions mention is known **statically**, from the file's own conditional facts — no walk
needed. So the key should be a digest of *that projection*:

```text
key  (file, digest of the values of the names this file's conditions actually read)
```

A root that defines `MY_FEATURE` before including `<string>` changes the environment but **not this projection**, so
the entry still hits. That is the same idea clangd uses when it hashes what a preamble *uses* rather than everything
in force, and it is the refinement this measurement bought.

### The rule that survives, and the one that does not

```text
survives     a condition is a pure function of (which #if, what it can read) — memoize on *that*
does not     "the same file is entered the same way many times" — it is not, and a key that assumes it is silent
             and useless rather than wrong: it never hits, so nothing is ever answered wrongly, and nothing is
             ever saved either
```

A cache that cannot hit is worse than no cache, because it is a cache that *looks* like the fix. The measurement
that settles it is one number — the hit rate or the wall clock — and it must be taken **before** the design is
believed, which is what §8's first step was for. It was taken, and the design changed.
## 10. Step 1 was run, as §8 said to — and it says **stop**

```text
EmmyLuaCodeStyle, 1122 files, one real entry file:
  walk census: 99,238 entries into 67,554 distinct (file, environment) pairs — 1.5x is repeated work
```

So the whole-environment key buys **1.5×**, not the 60× that §4.1 assumed. 99,238 − 67,554 = **31,684 entries** could be
skipped, and the splice machinery would be paid for on the other 67,554 — and on the *cost*, which is worse than that
ratio suggests, because the first entry of every distinct pair still walks the file in full.

§8 named this branch in advance:

> *a probe reports the hit rate … **If it is not high, stop and reconsider** — the rest is only worth building if the
> duplication is really there.*

It is not there, at this granularity. The 60× was the number of **visits**; the number of **reuses** is 1.5×.

### What this says about the three designs, in order

```text
per-condition key        measured slower than none (81 s → 138 s); the key changes on every #define
per-(file, environment)  measured 1.5x of the entries — not worth a splice machinery. **This is where the
                         document's plan stops**, and it stopped because a count was taken first
per-projection key       not measured yet. A file's conditions depend only on the names those conditions read,
                         so the key is stable across roots that agree on *those* — which is the case §9 argues for.
                         It is a refinement of the key above, and it needs its own count before anything is built
```

### The lesson, stated plainly

§8 existed because a design believed without a number is a design that gets built and then quietly does nothing. I
wrote §8, then built the cache, measured the wall clock, and **inferred** the hit rate from it — which is the same
mistake in a new coat: deciding from a shape instead of from the quantity that decides. Taking the count cost one
module, one call site and one run; skipping it cost a built-and-reverted cache and two rounds of reasoning.

The count is now taken, the plan stops where the document said it would, and one number (the projection's ratio) is
what stands between this design and being abandoned for the other half of the load — `[indexing]`, which was 34.6% of
the instrumented work and has nothing to do with the walk.
## 11. The projection key, measured — **37×**, and the design may proceed

§10 left one number standing between this design and its abandonment. It is now taken, with the key §9 argued for:

```text
EmmyLuaCodeStyle, 1122 files, one real entry file (`CodeFormatServer/src/LanguageServer.cpp`):
  census: entries 91,290   distinct (file, projection) 2,452   ratio **37.2**
```

against the whole-environment key's **1.5**. The difference is the whole point of the refinement: a root that defines
its own macros before reaching `<string>` changes the environment and **not** the names `<string>`'s conditions read,
so the entry hits.

**37 is a floor, not an estimate.** The projection is read by scanning the file's own text for the identifiers on its
`#if` / `#ifdef` / `#ifndef` / `#elif` lines — no walk, no summary, no coordinates — and that scan is deliberately
**wider** than the true dependency (it takes every identifier on the line, `defined` included). A wider key makes
fewer hits, so the true ratio is at least this.

The census is `crates/cpp_code_analysis/src/index/walk_census.rs`, switched on with `CPPLS_WALK_CENSUS=1` and printing
a line per entry so that "it did not run" cannot be read as "it found nothing" (§7's rule). It is kept rather than
deleted because step 2 has a correctness verification it can give: **after the splice, the number of entries walked
fresh must fall to the distinct count** — 2,452 — and the ratio must approach the entry count.

Verified by:

```text
cargo run --release -p cpp_code_analysis --example checks_probe -- E:\EmmyLuaCodeStyle <entry.cpp>
CPPLS_WALK_CENSUS=1   # and read the last `census:` line
```

### What remains, in the order §8 gives

```text
2. the splice        TimelineBuilder opens a frame whose subtree comes from the cache
3. the walk consults it   compare wall clock on the same project and entry; the census's distinct count is the
                          expected number of fresh walks
4. persistence       through the existing codec, so a restart starts warm
```
### 11.1 Step 2's design, settled by reading the structures

Two facts decide how the splice is written, and both were read out of `summary.rs` rather than assumed:

```text
TuFrame { file, parent, from_in_parent, visit_once, entry_seq, tout }
```

* **A frame's subtree is contiguous in both vectors.** Frames are pushed in depth-first order and a frame's events are
  `events[entry_seq..tout]`, so splicing is an index rewrite (add the base) plus a parent remap — which is what §4.2
  claimed and this is the field that makes it true.
* **`Walked.entries` cannot be cached.** It is `Vec<(usize, &'a MacroFact, &'a str)>` — borrowed from the summaries —
  so a contribution stored across walks must keep the **events** and re-derive the entries when it is spliced: an
  event already carries `name`, `at`, `frame`, the body text and the parameters, and the summary's own facts are
  reachable by `(file, at)`. `conditional_bodies` is re-derived the same way; `dead_macros` is owned
  (`HashMap<PathBuf, Vec<usize>>`) and is stored as it stands.

And one correctness condition that the splice must carry, which is *not* in §4:

```text
seen   the walk enters each file once per unit (the include-guard idiom, `visit_once`). A cached subtree therefore
       records what a file contributed **when it was first entered** — so before splicing, every file in the subtree
       must be one this walk has not already seen. If any of them is, walk the file fresh instead: conservative,
       correct, and the ordinary case is a fresh include tree anyway.
```

The cache itself is keyed by `(file, projection digest)` — the 37× key — with the projection computed by
`index::walk_census`'s scanner, which is already written and already the thing the ratio was measured with.
### 11.2 Step 2's edit list, as far as it is built

**Built and green** (`cargo build`, `cargo test`): `index/walk_cache.rs` with the `Contribution` type, the key, the
per-thread store, `rebase` and its two tests; `SummaryStore::revision()` with a bump and a cache clear in `forget` and
`invalidate`; `Clone`/`Debug` on `TuEvent` and `TuFrame`; `walk_census::{hash, projection_digest}`.

The rest, in the order the compiler will force:

```text
1. Walked.entries gains the file     it is (usize, &'a MacroFact, &'a str) today, and a contribution has to say which
                                     file each entry came from. One push site (summary.rs, the unconditional arm),
                                     and the consumers follow the compiler.
2. Walked gains `changes`            the ordered define/undefine list, pushed in the same two arms of the macro loop
                                     that already call unit.define / unit.undefine. A splice replays it into the unit.
3. the splice, in summary.rs         NOT in walk_cache: it touches UnitState, Walked and the `seen` set, all private
                                     there. It enters the root, rebases the frames and events, replays the changes,
                                     appends the entries/bodies/dead lists, adds the counts, and marks every file in
                                     `files` as seen.
4. the record, at the tail           capture (frames, events, changes, entries) lengths at the head, and at the exit
                                     build the Contribution from the ranges. Frames store `parent`, `entry_seq` and
                                     `tout` **relative** to the head's bases; the root's `parent` is replaced at splice
                                     time by the site's.
5. the precondition                  a splice only when none of `files` is already in `seen` — the include-guard
                                     condition, see 11.1.
```

Then the verification §11 already names: the census's fresh entries must fall toward **2,452**, and the cold-load wall
clock on the same entry file must fall with it. The cache is per thread, so a single-threaded pump gets all of it and
a parallel one gets a share; that is deliberate — this round is about the work being done once, not about how many
cores do it.
### 11.3 The first splice was built, measured, and was **wrong in its granularity**

Built: a `(file, contents, projection)` keyed cache of a file's **whole subtree** — frames, events, state changes,
evidence positions, bodies, dead macros — spliced by rebasing indices and replaying the changes. The suite stayed green
(53 targets ok, the pre-existing latency test red), so it is semantically plausible; the numbers say it is the wrong
shape:

```text
EmmyLuaCodeStyle, same entry file, same probe:
  spliced 0      fresh 69,089        ← the cache never hit
  wall 216.5 s                       ← against ~103 s with the cache off
```

Two causes, and only the second is a defect of the *number* rather than of the design:

* **A subtree per file is quadratic.** Every file of a chain of depth *d* stored *d* copies of its subtree, so the
  bookkeeping alone cost more than the walks it was meant to save. §4.2 says "a file's contribution is its frame
  subtree" — but §4.3 keys the cache **per include edge** and splices *recursively*, which is what keeps each stored
  contribution to one file's own frames.
* **The key's `contents` half was the text `look_up` answered with**, which is the rendered reading in one unit and the
  written one in another — so the same file hashed differently and the lookup could not repeat. The file's identity
  must come from the summary it was read from (its `SummaryKey`), not from a string the caller happens to hold.

Switched off behind `const SHARE: bool = false` in `summary.rs` with these numbers beside it, because leaving a cache
that is slower and never hits is worse than leaving none. The next step is the one §4.3 actually specifies:

```text
per-file contribution   the file's own frames (not its subtree) + the edges it took, each with the child's key
splice                  enter the frame, replay the changes, then recurse the edges — a memoized DAG traversal
key                     (the summary's own identity, the projection) and nothing taken from the caller's text
```
### 11.4 Round 3: the shape is corrected, the wiring is not yet in

The contribution was reshaped to what §4.3 specifies — **one file**, not a subtree:

```text
Contribution { file, visit_once, events (this file's own), changes (this file's),
               edges: [(offset in this file, the child's key)], bodies, dead, counts }
```

and `rebased` became one frame plus its events (`walk_cache`'s test says so: the frame takes the site's parent and
offset, the events take the frame's id, `tout` is left for the caller's `leave`). `Walked` gained `changes` and
`spliced`, the state-change type carries the file so a contribution can keep only its own, and `walk_one_file` was to
return the key it filed under so that the parent could record an edge.

**What is not in yet, and why it was not forced:** the walk's own wiring — the head's lookup, the recursive splice, and
the tail's record — was written for the subtree shape and does not survive the reshaping in the same round. Rather than
leave a half-rewritten walk on the hottest path, `summary.rs` was restored and the two new modules kept, compiling and
tested, behind `#![allow(dead_code)]` with this note. The suite is as it was: **53 targets ok**, one pre-existing red.

The remaining edits, in order, are exactly the ones this round settled:

```text
1. walk_one_file returns Option<u64>          the key it filed under, so a parent can record an edge
2. the include site pushes (offset, child key) into a local `edges` vec
3. the head: key = (path, projection); lookup; if it hits and the file is not already spliced, recurse-splice and
   return the key. `SHARE` is the constant that turns it on, and it stays `false` until the numbers are in
4. the tail: build a Contribution from the *ranges within this frame* — events whose `frame` is this one, the changes
   whose file is this one, the edges vec, the dead offsets, the bodies rebuilt from this file's own events — and store
   it. Per-frame counts need the same treatment: `conditional_facts`/`facts_in_force` are unit-wide totals today and a
   contribution must carry only this file's share, or a spliced unit's evidence density will read low.
5. the splice recurses on the edges with a depth cap, entering one frame per file, and `leave`s it after the edges.
```

The verification is unchanged and takes one run: `spliced` should approach 91,290 − 2,452, `fresh` should approach
2,452, and the wall clock must beat the 103 s baseline — the number §11.2's census line already prints.
### 11.5 Round 4: the splice is wired and **the order is wrong**

The per-file shape from §11.4 is wired: `walk_one_file` returns the key it filed under, the include site collects
`(offset, child key)`, the head computes `(path, projection)` and splices on a hit, the tail records this file's own
events, changes, edges, dead offsets and per-frame counts, and the splice recurses the edges with a depth cap.

With it live, **one** test failed:

```text
a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file     FAILED
```

and the cause is exact rather than mysterious: a contribution keeps its **state changes** and its **include edges** in
two lists, so the splice replays every change and only then recurses the edges. The walk interleaves them **by offset**
— one loop over macros and includes, whichever the file writes first — so a `#define` written after an `#include` must
take effect after the subtree that include pulled in. Replaying it before is a different program.

**The fix, which the next round is one step away from:** a contribution's events, changes and edges are all in
**offset order already** (the walk visits them in that order), so the splice merges the three by offset and does each
thing where it belongs — an event where its fact was, its state change with it, and an edge where the include was.
That needs one field: `Undefined` must carry its offset too, so the change list can be merged by the same key.

`const SHARE: bool = false` sits in `summary.rs` with these numbers beside it, and the suite is back to
**53 targets ok** with only the pre-existing latency test red.
### 11.6 Round 5: two divergences fixed, a third still open

With the splice live, one semantics test fails — `a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file`:
a macro in a header expands to a struct declaration, and the cooked reading has to find it. Two real divergences were
found and fixed on the way, both of which would have been wrong answers in production:

```text
1. order      the splice replayed every state change before recursing the edges. The walk interleaves them **by
              offset** — a `#define` written after an `#include` takes effect after it. Now the three lists (events,
              changes, edges) are merged by offset, which needs `StateChange::at`; `Undefined` carries it now.
2. seen       the recursion spliced a child the unit had already read, duplicating its evidence. The walk enters a
              file once per unit (the include-guard idiom), so the recursion now checks `seen` and the `spliced` set
              before descending.
```

Neither fixed the test, so the third divergence is elsewhere. `const SHARE: bool = false` is back, the suite is
**53 targets ok** with only the pre-existing latency test red, and the bisect is the next round's work — one channel at
a time, because the splice fills four of them and only one can be wrong:

```text
a. the state        replay only the changes (no evidence, no bodies, no dead): if the test passes, the divergence is
                    in a channel, not in the state
b. the entries      add the evidence rebuild; the test spot-checks `definition("HWND__")`, which reads exactly this
c. the bodies       the second channel — `conditional_bodies` — which a conditional definition feeds
d. the dead         `dead_macros`, which decides which `#define`s the cooker treats as not compiled
```

The suspicion, stated before the bisect so it can be wrong: (b). The evidence rebuild finds its fact with
`binary_search_by_key(at)` and pushes `(from_offset, fact, source)` — and the walk pushes the *same* tuple from the
loop that fed the state, in the same order. If those two are not the same list, the rebuilt one is missing something the
walk had, and a test that asks "is this declaration known" would fail exactly this way.
### 11.7 Round 6: the third divergence is real and is **not** where it looked

The bisect was made cheap by running **one test** (`--lib a_declaration_only_a_macro_makes` — 0.01 s), which is what
turned this round from four suite runs into four one-second runs. Three findings:

```text
traced, with `CPPLS_SPLICE_TRACE=1`:
  record: /p/api.h    | 0 events, 0 changes, **0 edges**     ← the include was not recorded
  record: /p/handle.h | 1 events, 1 changes, 0 edges         ← the child was walked, correctly
  splice: /p/api.h    | (the empty contribution, trusted)
```

so the first unit for `api.h` ran while `#include "handle.h"` was **unresolved** (`resolved: None`), recorded a
contribution with no edges, and a later unit with the include resolved hit it and lost the subtree. A guard was added —
**a file with any unresolved include is not cacheable**, because that fact belongs to the summary and not to the walk,
so the invalidation clock cannot cover it:

```rust
let resolvable = !file.includes.iter().any(|include| include.resolved.is_none());
```

**Measured after the guard: the test still fails.** That is the useful part: the stale-contribution story was true and
is now impossible, and it was not the whole cause. The remaining candidate is the one channel the trace did not look at
— the **evidence rebuild inside the splice**: the test asks `definition("HWND__")`, which reads exactly the macro
evidence, and `handle.h`'s contribution is the one that carries `DECLARE_HANDLE`. The next instrumentation is to print,
in the splice, the number of entries rebuilt **and** whether the fact was found by `binary_search_by_key(event.at)` —
if the lookup misses, the splice is silently dropping the evidence the walk would have pushed, and the walk and the
splice disagree about a summary's own offsets.

`SHARE` is `false` again, the three fixes stay (they are correct on their own terms), and the suite is 53 targets ok
with only the pre-existing latency test red.
### 11.8 Round 7: pinpointed to one line — `look_up` answers `None` for the child

Instrumenting every return of `walk_one_file` with `CPPLS_SPLICE_TRACE=1`, one trace settles it:

```text
enter: /p/api.h    | already seen false, spliced false, timeline true
enter: /p/handle.h | already seen false, spliced false, timeline true     ← the child is entered
child key: None                                                            ← and returns None
tail: /p/api.h     | resolvable true, bases true, frame true, timeline true
record: /p/api.h   | includes 1 (resolved 1), edges 0, events 0
```

There is **no** `tail: /p/handle.h` for that call: the child never reaches its own tail, so it returned from one of
the three early exits. `seen`/`spliced` is excluded by its own line (`already seen false, spliced false`), and the
splice's success path returns `Some`. **The remaining one is `look_up(&path) -> None`** — and that explains everything
else in the trace: the child contributes nothing *in this unit*, so the parent records a contribution with zero edges,
and a later unit that hits it loses the subtree. The child's own `record:` line comes from a *different* invocation,
which is why it appears after the parent's.

That is the third divergence, and it is not in the splice at all: it is in what `look_up` is allowed to answer inside
the recursion. The next round's first question is therefore written down rather than guessed:

```text
why does look_up(child) answer None here?
  a. the closure the unit was built with is restricted to the root's closure as the *index* knew it, and the child
     was resolved later — the same shape as §11.7's stale-edge bug, one layer down
  b. the closure is `&mut impl FnMut` and a splice consumed it — unlikely, it takes `&mut` only
  c. the child is not in whatever set the caller's look-up walks — a session question rather than a walk one
```

`SHARE` is `false`, the traces are removed, the three real fixes stay (interleave by offset, respect `seen` on the
recursion, do not cache a file whose includes are unresolved), and the suite is **53 targets ok** with only the
pre-existing latency test red.
### 11.9 Round 8: the sharing is **live and green**, and the first measurement said it was slower

The third divergence was the one §11.8 pointed at, one level up from the walk: **`look_up` can only answer for files
the closure holds**, so a unit built while the closure was still growing is incomplete by construction. `Session` already
names that a *provisional* unit and refuses to store it; the sharing cache had the same reason and had not been told.
`share` is now a parameter of `TranslationUnit::walk` (the caller's answer, because the closure is the caller's), and
`session` passes `!provisional` — asked **before** the walk, since the walk needs the answer.

With that, **the whole suite passes with the sharing on**: 53 targets ok, and the only red is the pre-existing latency
test. Three real correctness fixes came out of the work, all of them independently right:

```text
interleave by offset     events, changes and edges merged by offset, not changes-then-edges
seen on the recursion    a spliced child the unit already read is not spliced again
provisional              an incomplete unit neither fills nor reads the cache
unresolved includes      a file whose includes are not resolved is not cached (its edges would be empty)
contents in the key      `a_completion_sees_a_change_to_an_included_header` caught the key without the file's own
                         fingerprint: the clock covers a dropped summary, not a header rewritten underneath one
```

And the first real measurement of the win:

```text
EmmyLuaCodeStyle, same entry file, same probe:
  spliced 2,085   fresh 998        ← 98.7% of the file entries were answered from the cache
  wall **159.1 s**                 ← against ~103 s without it. Slower.
```

**The key cost more than the work it saved.** `projection_digest` scanned the file's text on **every entry** — and
leaked every identifier it saw (`Box::leak`). Fixed: the scan is memoized per file behind an O(1) fingerprint
(`index::walk_census::projection_of`), the leak is gone, and the memo is cleared together with the contributions
(`walk_cache::clear`), because the same edits invalidate both. The wall clock of that fixed version is the next round's
first measurement — it has not been taken yet, and this document does not claim it.

The lesson is §8's, one layer down: the count that justifies a design is not the count that makes it pay. 37× was real
and the design was right; the *implementation of the key* was the thing that had to be measured, and it was, before
anything was claimed.
### 11.10 Round 9: the sharing is **faster, measured A/B on the same day** — and the old baseline was fiction

The first thing this round did was the thing the last one should have: **measure the baseline again**. The 103 s figure
that every earlier comparison used came from a build two commits old, and quoting it against today's code was a
measurement error of exactly the kind §8 exists to prevent.

```text
EmmyLuaCodeStyle, LanguageServer.cpp, checks_probe, same machine, same hour:

  sharing OFF (CPPLS_NO_SHARE=1)   166.5 s   indexed in 129.3 s
  sharing ON                       152.9 s   indexed in 115.9 s
                                   ↓ **−8.2%** of the whole probe, −10.4% of the index

  spliced 2,085   fresh 998         ← 98.7% of the file entries answered from a contribution
```

So the design pays, and it pays **about eight percent**, not the order of magnitude the 37× suggested. That is not the
cache failing; it is the walk no longer being the whole load. The measurement is now an A/B switch kept in the tree
(`CPPLS_NO_SHARE=1` in `session.rs`, with these numbers beside it), because a comparison against a remembered number is
not a comparison.

The honest position: the machinery is correct and green (53 targets ok, one pre-existing red), the reuse is real
(2,085 splices, 998 fresh walks, against 76,473 entries), and the wall clock moved in the right direction by a modest
amount. **What this round does not know is where the other ~90% of the load goes** — the walk is no longer it. The next
measurement is a fresh decomposition of today's 130 s index, not another walk optimisation.
### 11.11 Round 10: step 4's value, and why it is smaller than it looks

The objective's remaining item is persistence ("so a restart starts warm"). Reading the two caches before building it
changed what it is worth:

```text
the summary cache     one file per summary, keyed by contents — already warm across restarts, and the plan before the
                     sharing work: "cold 46.9 s / 64.7 s, warm 6.7 s" (`docs/cold-start-cpu.md`)
the unit cache        `.cppls`, keyed by (path, context, root key) — already persists `TranslationUnit`s, which is what
                     the *cook* path reuses
the sharing cache     in memory only, per thread — it exists for the **first** load, which is what it accelerated
                     (§11.10: 166.5 s → 152.9 s)
```

So a restart is already warm for the paths the two persisted caches cover, and the sharing's own persistence would add
warmth for what they do **not** cover: the **second pass** and the cook path's re-walks. That is real but narrow, and it
is the honest reason this step was not hurried.

**The design is settled**, and it is simpler than it first looked: a contribution's `changes` need not be written,
because every one of them is implied by the events — a definition is an event with a body, an `#undef` one without —
and the splice can rebuild them (it is the splice that has the unit's `definitions_of` and `look_up`). What goes on disk
is then exactly the plain data: key, file, `visit_once`, events, edges, dead offsets, bodies, two counts — magic and a
count in front, the same conventions as `summary_codec`.

Implementing it this round was attempted and rolled back: the block was appended to `walk_cache.rs` and the file's test
module was left unbalanced by the surgery, which is a reason to write a new file rather than to append to a large one.
The tree is green again (53 targets ok, one pre-existing red), and the next attempt starts in a new module —
`index/walk_cache_codec.rs` — with the round-trip test as its first inhabitant.
### 11.12 Round 11: step 4 is built — the codec, its tests, and the lifecycle hooks

`index/walk_cache_codec.rs` is a new module, which is what §11.11 said the next attempt should do: a new file rather
than an append to a large one, with the round-trip test as its first inhabitant. It writes exactly the plain data — key,
file, `visit_once`, events, edges, dead offsets, bodies, two counts — with a magic and a count in front, and it does
**not** write the state changes: they are implied by the events and rebuilt at a splice, which keeps an `Arc<MacroDef>`
out of the format and one less thing able to disagree with the events.

Five tests, all green:

```text
index::walk_cache_codec::tests::a_cache_survives_the_round_trip
index::walk_cache_codec::tests::a_foreign_or_truncated_file_is_refused_rather_than_half_applied
index::walk_cache_codec::tests::an_empty_cache_is_a_valid_cache
index::walk_cache::tests::a_spliced_frame_takes_its_parent_from_the_site_and_its_events_take_the_frame
index::walk_cache::tests::the_key_moves_with_the_file_its_contents_and_the_projection
```

A truncated file is refused rather than half-applied — the one failure mode worth writing a test for, because a cache
that half-loads answers some units from a stale contribution and some from a walk.

The lifecycle is wired in `handlers/initialized/mod.rs`: the cache is **loaded before the first file is read** (so this
run's first units can splice) and **written after the workspace drain** (so it holds everything the run learned). Both
are one `update_session` call, and both log — "restored N" and "wrote N" — because "it loaded nothing" and "it did not
run" are different facts.

**What is still unverified:** the hooks have unit tests but no end-to-end run. The honest check is a server load on the
real project twice — cold writes, warm restores and is faster — and it has not been done.
### 11.13 Round 12: step 4 verified end to end, and the design closes

The persistence had unit tests and no end-to-end check. It has one now, and it is the kind that would have caught the
mistakes of the last four rounds:

```text
the_sharing_cache_survives_a_restart
  a run that walks files writes N contributions        (N > 0)
  the memory is cleared                                (held() == 0)
  a new session over the same cache directory reads N  (restored == written)
  and they are in the cache the walk consults          (held() == restored)
```

The last assertion is the one that matters: a cache restored into a side table and never consulted would pass the first
three.

**The design, closed:**

```text
§8 step 1   the projection key's reuse        **37×** (91,290 entries into 2,452 distinct pairs)
§8 step 2   splicing, keyed per include edge  live, 2,085 splices against 998 fresh walks
§8 step 3   the walk consults it              `share` is the caller's answer; session passes `!provisional`
§8 step 4   persistence                       `index/walk_cache_codec.rs`, loaded before the first read and written
                                              after the drain, verified across a restart
```

**What the work cost, and what it bought**, stated without rounding up:

```text
bought     −8.2% of a cold load, measured A/B on the same machine in the same hour (166.5 s → 152.9 s),
           and 98.7% of the file entries no longer re-walked
cost       five rounds of divergence-hunting — interleaving by offset, the include-guard rule on the recursion, the
           provisional unit, an unresolved include, a key without the file's contents — every one of them a wrong
           answer that the tests could see and the design documents had not predicted
learned    a design's justifying ratio is not its payoff: 37× of the entries is −8% of the wall clock, because the
           walk had stopped being the whole load. Both numbers were needed, and the second one was not in any plan.
```

The step that is **not** in this document, and should be next, is the fresh decomposition of today's remaining ~117 s:
the walk is provably no longer it.
### 11.14 Round 11: the interleaving is **in**, and the test is still red — the next divergence is elsewhere

The splice now merges its three lists **by offset**: a file's state changes, the evidence it wrote, and the include
edges it took are one sequence in the source, and the walk visits them in that order. The loop pops the smallest offset
of the three, applies the changes there, pushes the evidence there, and recurses the edge there. `StateChange` carries
its offset for exactly this (`Defined.at`, `Undefined.at`).

Measured with the sharing on, the same test still fails:

```text
a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file   FAILED
```

So the ordering was a real defect — two of the three divergences so far were — and it was not this one's cause. The
sharing is off again (`const SHARE: bool = false`), the suite is back to **53 targets ok** with only the pre-existing
latency red, and the merge stays in the tree because it is right on its own terms and the ordering it fixes is
observable in principle (a `#define` written after an `#include`).

**What the next probe should ask**, in this order and each in one run of the one-second test:

```text
1  does the contribution for `handle.h` hold its `DECLARE_HANDLE` event at all?   print `contribution.events.len()`
   and each event's name/at/unconditional when the record happens
2  is that same event present in the spliced unit's timeline?                     print the events the unit ends with
3  does the unit's `entries` hold it after the splice?                            print `walked.entries.len()` before
                                                                                  and after `splice_a_contribution`
```

One of those three is where the walked unit and the spliced unit differ, and each is a line of instrumentation rather
than a hypothesis.
