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
