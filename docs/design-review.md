# The design, reviewed against what it turned out to cost

Written after the indexing round that took a 126-file closure from **2403 ms to 919 ms of wall clock** and named what
is left. It is a review rather than a plan: what the architecture gets right, what it gets wrong, and which of the
wrong things are worth changing next. Numbers are from `docs/indexing-performance.md` and
`docs/incremental-edits.md`, and every one of them was measured on this machine against MSVC 14.35's standard library.

---

## 1. What the design is

```text
  cpp_parser          a hand-written recursive-descent parser over rowan: a LOSSLESS green tree, every token and
                      every piece of trivia, with the ranges an editor needs
  cpp_code_analysis   the reading layer:
      SummaryStore        one summary per file, keyed by the file's content hash + the configuration, cached on disk
      ProjectIndex        the summaries as a queryable whole (names, scopes, includes, visibility, modules)
      TranslationUnit     a macro timeline over one root's whole include closure, cached on disk
      cook                the file read the way a COMPILER reads it: lex, expand, render, parse the rendering
      Session             the state machine over all of it: a work queue, a cooking queue, the sessions's own edits
  cpp_ls              the LSP shell: request handlers, a pump, a read/write lock discipline
```

The idea that distinguishes it from clangd is that **a file's meaning depends on its closure's macros, and the closure
is modelled explicitly**. A `TranslationUnit` walks the whole include closure once and records a *timeline* — every
`#define`, every `#if`, every file entry, in order — so "what is `_STD_BEGIN` in `<xstring>`" is a positional lookup in
that timeline instead of a walk. MSVC's standard library is written entirely with macros that open namespaces, so this
is not a hypothetical concern: without it every declaration in `<vector>` lands at file scope.

---

## 2. What it gets right

**2.1 The summary is the right unit of persistence.** A per-file, content-addressed summary is what IntelliJ's stub
trees and clangd's index shards are, arrived at independently. `StoreStats { reused, rebuilt, unstored }` makes the
cache's behaviour observable, which is why "a warm start re-reads and does not re-parse" is a claim with a number
behind it rather than a hope.

**2.2 The stage instrumentation is the best thing in the crate.** `Stage`/`StageTimes`/`Stage::entries` turned four
separate performance questions this round from arguments into measurements, and two of the four answers were *not* what
anyone expected. A codebase that can answer "where did that second go" in one command is a codebase that can be
improved; one that cannot is improved by guesswork.

**2.3 The raw/cooked split is a real distinction, not a cache.** "What is written here" and "what is compiled" differ,
and the crate names the difference instead of blurring it: `declarations_in` answers the first, `cooked_declarations`
the second, and `Session::diagnostics` chooses deliberately. Most language servers have one answer and are wrong in one
of the two directions.

**2.4 Failure is not exceptional.** Unreadable files, unresolved includes, parse errors and truncated closures are all
*values* — `Unstored`, `NotIndexedReason`, `UnknownReason` — rather than errors. That is why a partially indexed project
still answers questions, and it is what makes the LSP layer's `isIncomplete` honest.

**2.5 The comments are load-bearing.** Nearly every non-obvious decision in this crate carries the measurement that
produced it and the alternative that was rejected. Several times this round the fastest path to a diagnosis was reading
a comment that already contained it — `DeclarationShapes::of`'s doc named the exact defect `pattern_of` still had.

---

## 3. What it gets wrong

### 3.1 The cost model was never checked against the work actually done — **being fixed**

The dominant pattern of every defect found this round is one shape:

> **A per-declaration walk for an answer about the file.**

```text
  pattern_of              a descent from the root per declaration                → 2245 ms, now a table lookup
  enclosing_namespaces_of a scan of every scope per local declaration            → 1576 ms, now once per scope
  pattern_of's text       the whole class materialised per MEMBER                → 6845 → 4613 ms
```

Each was written correctly, tested, and left a timer on the *function* rather than on the *question*. The fix in every
case was the same one the crate had already invented next door (`DeclarationShapes`, a table built in one pass). **The
design has the right primitive and did not use it consistently.**

### 3.2 The unit of an edit is the closure, and it should be the file — **partly fixed**

`SummaryKey` is the file's own text plus `context_hash`, which is right. But a *timeline* is keyed on every file the
walk entered, so before this round **any keystroke in a file invalidated that file's own timeline** — the root was
hashed whole — and every cook of it re-walked its closure. `RootKey` fixed exactly that (the root is keyed on its
preamble). What remains coupled:

```text
  context_hash          a change to the compiler configuration invalidates everything, correctly
  the closure's hashes  a change to any included file invalidates the timeline, correctly — but the invalidation is
                        checked by hashing every file rather than by a `stat`, so a warm start reads the whole
                        closure to decide it did not need to
```

**The stub-tree invariant from IntelliJ is the missing rule**: *"all information stored in the stub tree depends only
on the contents of the file for which stubs are being built"*. Ours depends on the closure by design — that is the
point of the timeline — but the *summary* should not have to, and today `context_hash` is the only thing keeping a
summary from being purely a function of its file.

### 3.3 The tree contains everything, and most of it is not needed — **built, measured, and worth nothing**

```text
  internal nodes   754 604   inside a statement block  486 103   64.4%
  tokens         1 377 789   inside a statement block  853 387   61.9%
```

All three reference tools refuse to build syntax for function bodies while indexing:
`SkipFunctionBodies = true` (clangd), `skipChildProcessingWhenBuildingStubs` (IntelliJ), *"it skips the content of
blocks … and ignores its contents"* (VS). **64% of the node traffic is bodies** — so this section said, and it was
right about the nodes.

**It was wrong about the cost.** A parse mode that flattens every statement block was built, tested and measured, and
the index is **no faster**: wall 830 → 821 ms, `parse` −16%, `sweep` *+10%*, and three capabilities lost. See
`docs/indexing-performance.md` §3.4 for the table. The mechanism is in `ParserConfig::skip_bodies`, disabled, with the
numbers in its documentation.

**The reason is the one fact this review should have started from**: building nodes is about **a quarter of `parse`**,
and the stages that dominate are per-**declaration**, not per-node. `sweep` and `facts` walk to each declaration and ask
questions about it; they do not care how deeply the statements around it were nested. The correct statement of this
codebase's cost model is:

> **The cost is proportional to the number of declarations, not to the size of the tree.**

Every measurement in the indexing round agrees with it, including the two items that *did* work — both were
per-declaration costs, and between them they were worth 36% and 35% of the wall clock.


### 3.4 One tree, two consumers, no mode — the blocker for 3.3

The indexer wants bodies skipped; `Session::view` — the file the user is looking at — wants them whole, because that is
where inlay hints, semantic tokens, deduced return types and local references come from. Today both get the same tree
from the same parse. clangd resolves this by having **two different artifacts**: a background/static index that never
sees a body, and a full AST for the open file. We have the same two consumers and one artifact.

### 3.5 The shell owns decisions that belong to the analysis layer

`docs/incremental-edits.md` §8.7 is the worked example: stage 4 of the latency plan — *stop rendering the edited file* —
was dropped because the rendering of the edited file is not produced by an eager cook at all. It is produced because
`cpp_ls/src/context/analysis_state.rs:167-174` asks for it **on every request that names the file**. A cost decision
made in the layer above cannot be reasoned about from the layer below, and it was only found by grepping for the caller.

### 3.6 Readiness is per-session, and should be per-capability

`is_ready_for_a_request_about` and `isIncomplete` are two coarse signals. IntelliJ's `DumbService` makes it a
per-feature property (`DumbAware`), so completion and highlighting keep working while index-dependent navigation does
not; VS has the same split between its live engine and its database-only features. Our equivalent is one flag, and the
pump's `WRITE_BUDGET`/`QUERY_BUDGET` are what stand in for it.

---

## 4. What is *not* wrong, though it looks like it

**4.1 "We are slower because we parse twice."** The cook adds **0 declarations** to a `.cpp` (measured, `cook_value`)
and 262 to `<vector>`. The dual reading is not the cost; the per-declaration walks were.

**4.2 "rowan is the problem."** `parse` — where the tree is *built* — is 39% of the CPU, and the rest is *walking* it.
Replacing the tree attacks the smaller half and costs the parser plus every consumer plus the lossless-range property
the whole layer rests on. §3.3 changes **what the tree contains**, which is the measurement that would justify
revisiting the tree library — and it may make the question moot.

**4.3 "clangd indexes instantly."** Its own documentation says a large project's background index takes *"multiple
hours even on very powerful machines"* and *"multiple GB of RAM"*. All three tools are O(closure) on the first index.
What they avoid is (a) building nodes for bodies and (b) redoing it per keystroke.

---

## 5. Where the time is now, and what to do about each

```text
  919 ms wall / ~3780 ms CPU for 126 files of MSVC's standard library   (~30 ms CPU per file)

    parse     1138.7 ms   39%   ← 64% of it is bodies' nodes (§3.3)
    sweep     1182.4 ms   40%     of which facts 721.9
    units      173 ms      5%     the closure timeline: the part clangd does not have and we would not give up
    encode     101 ms      3%     writing summaries
    include-scan 206 ms    5%
```

**In order of leverage:**

```text
  next    —           **nothing on this list is a known win any more.** §3.3 was the architectural item and it was
                      built and measured at zero. §3.2 (stat-based closure validation, then mmap) is the one
                      remaining change with a mechanism behind it, and it needs its own ceiling measured before it
                      is attempted — the same way §3.3's was, and for the same reason: the last two "obviously the
                      biggest lever" items were both wrong.

  then    3.2         `stat`-based closure validation, so a warm start does not read the whole closure to decide
                      nothing changed; then mmap the artifacts. **Measure the ceiling first**: how much of a warm
                      start is hashing and reading?

  then    3.6         a per-capability readiness signal, so a cold index degrades features instead of stalling them

  later   3.5         move the "does this need a cooked reading" decision out of the LSP layer, or accept that it
                      lives there and document it where the cost is reasoned about
```

**What not to do**: skip bodies while indexing (§3.3 — built, measured, zero, and it costs three capabilities); skip
local facts (measured at 82 ms of `facts` and no wall time at all); replace rowan (§4.2); and any further "make this
walk faster" before finding out *which* walk, by ablation with the cache cleared, because three plausible hypotheses in
a row were wrong.

**The honest state of the performance work**: 2403 ms → 919 ms of wall clock for a 126-file closure (−62%), from two
changes, both of which removed a per-declaration walk. Nothing else that has been tried or measured since has moved it
at all. The next step is not a bigger idea; it is finding the next per-declaration cost, and the way to find it is the
ablation that found the last two.


---

## 6. The two rules this round earned

**6.1 A stage whose remainder is 96% of itself is a label, not an instrument.** `facts` was 6845 ms with five timers
inside it summing to 230 ms, and the plan's first item was built on a *guess* about the other 6600. The guess was wrong.
A timer on a function is not a timer on a question.

**6.2 Every plausible hypothesis this round was wrong at first.** `is_clean`'s linear scan: nothing. Locals in inline
bodies: nothing. The first ablation reported `is_clean` as the entire cost because it compared a warm run against a cold
one. The three real findings all came from **ablation with the cache cleared** — stub one call, run, compare — and none
of them came from reading the code, including the two the reading had already convinced me of. The instruments are in
the repository now (`workspace_probe`, `keystroke`, `cook_value`, `body_weight`), and the cost of running one is a
minute; the cost of the three wrong guesses was most of a day.
