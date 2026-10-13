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
