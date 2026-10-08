# Indexing performance: what the other tools do, what we do, and what to change

This document exists because the question was *"do clangd / CLion / VS have a secret we can learn?"* The answer is
yes, there are three, they are architectural rather than micro-optimizational, and **two of them are about not doing
work rather than doing it faster.** It also records the measurement that says which of them we are missing.

---

## 1. The measurement, first

```text
cargo run --release -p cpp_code_analysis --example workspace_probe -- <dir>
```

A translation unit including `<algorithm> <map> <memory> <string> <unordered_map> <vector>`, on this machine, against
MSVC 14.35.32215 with 8 system include directories. **126 files** are reached and indexed. `Stage` counters are CPU
time summed across the workers; wall clock is separate and is quoted beside them.

```text
                          before          after
  wall                    2403.9 ms       2219.7 ms
  CPU (stages total)      8862.6 ms       6669.4 ms     −25%
    read                    76.3            25.2
    include-scan           186.9           205.5
    parse                  996.7  11.2%   1089.9  16.3%
    sweep                 7293.1  82.3%   5028.6  75.4%   ← the whole problem
      scopes               242.2           246.2
      facts               6845.0          4612.9
        type-of            104.4                          the five questions `fact_for` is *named* for
        returns            117.0
        bases                2.4
        template-params      5.6
        alias                0.5
        …everything else   ~6600         ~4400           untimed
      includes              86.7            85.9
      guards                 3.0
      scan                  74.6            81.7
      drop                  38.7
    encode                  81.8           100.9
    walk                   153.3           146.9   ← the closure walk is 2% of this
```

**Reading this correctly matters more than the numbers.**

0. **§3 is what happened next, and it changed the picture.** Two calls — `pattern_of` and `parameter_list_at` — are
   84% of `facts` and 58% of the CPU, and neither had a timer. This table's `facts` row was *un-attributable* when it
   was written, and the paragraph below it was a guess about why. The guess was wrong; the ablation in §3 is what
   settled it. The lesson is not "add more timers" but the narrower one: **a stage whose remainder is 96% of itself is
   not an instrument, it is a label.**

1. **Parsing is not the bottleneck.** `parse` is 11–16% of the CPU. The question "can we parse faster" has an answer
   worth at most 16%, and a parser rewrite is the most expensive way to buy it.
2. **`sweep` is 75–82%, and inside it one function is ~69%**: `build_facts`, which turns the tree into `DeclFact`s.
3. **The five sub-stages that exist inside that function add up to 230 ms of 6845.** The stage instrumentation was
   pointed at the four *type* questions — because they were once 98% of the sweep on a different corpus — and the
   remaining 96% of the function had no timer at all. *A stage table tells you where the time was when it was
   written, not where it is now.*

**Per file this is ~53 ms of CPU to index a file of MSVC's standard library.** A stub builder costs about a
millisecond. That is the gap, and it is not a constant factor on a good algorithm — it is a different algorithm.

---

## 2. What was actually wrong, twice

### 2.1 `pattern_of` materialised the whole class, once per member — **fixed**

```rust
// crates/cpp_code_analysis/src/sema/declarations.rs, before
let declaration = enclosing_class_of(root, binding)?;
let text = declaration.text().to_string();      // the WHOLE class, body included
```

`pattern_of` runs once per declaration, and this line built the enclosing class's entire text — for MSVC's
`basic_string`, a body of about a hundred kilobytes — **for every one of its couple of hundred members**. That is
`members × class size`, tens of megabytes of string building for one class, in a file (`xstring`) full of such
classes.

The fix is `class_head_of`, which walks the declaration's children and stops at the body *without taking its text*.
It is not a different answer: the caller only ever reads the head — the keyword, the name and the template argument
list, everything before the first `{` or `;` — so the part being thrown away was the part being built.

**Measured: `facts` 6845.0 → 4612.9 ms, CPU total 8862.6 → 6709.2 ms.**

### 2.2 Body scopes were walking their scope chain — **fixed, and it bought nothing**

`build_facts` asked `qualification_prefix_of(scope)` and `declares_a_local(scope)` for **every scope in the file**,
and both walk the scope chain. For `Function`, `Block` and `Lambda` the answers are fixed by the functions' own
`match` arms — a declaration in a body is local, and is qualified by nothing — and most scopes in a header are
bodies. The rewrite asks the cheap question first.

**Measured: 6669.4 vs 6709.2 ms — 40 ms, within noise.** It is in the code because it is provably the same answer
and removes work that cannot matter, but it is recorded here as *not a win*, because a change that was measured and
did nothing is exactly the kind of thing that gets remembered as a win later.

---

## 3. Where the remaining 4.4 seconds are — measured, not hypothesised

The hypothesis this section used to carry (locals in inline bodies) was **wrong**, which is worth recording because it
was the third plausible guess in a row to be wrong. What settled it was **ablation**: stub one call at a time and read
the profile. Each row below is a cold run — the cache directory deleted first, because the first attempt at this
measured a *warm* run and reported that `is_clean` was the entire cost, which it is not.

```text
  ablated                        wall        facts       delta
  (nothing)                      2207.3 ms   4562.4 ms
  is_clean → true                2225.7 ms   4609.9 ms   nothing
  access_at + exported_at        2236.3 ms   4643.6 ms   nothing
  pattern_of → None              1376.8 ms   2317.6 ms   −2245 ms   49% of facts
  parameter_list_at and
    enclosing_namespaces_of      1634.7 ms   2986.5 ms   −1576 ms   35% of facts
  …both together (= `facts`
    without its remainder)        ~766 ms      ~404 ms   −4216 ms
```

**Two calls are 84% of `facts` and 58% of the whole index's CPU.** Neither had a timer; both are per-declaration.

### 3.1 `pattern_of` — moved onto the shapes table, measured

```text
                        before      after
  wall                  2202.3 ms   1409.3 ms   −36%
  CPU (stages total)    6595.8 ms   4481.1 ms   −32%
    sweep               5002.8 ms   2818.2 ms   −44%
      facts             4562.4 ms   2365.0 ms   −48%
```

— which is the ablation's prediction to within noise (`pattern_of → None` gave `facts` 2317.6 and wall 1376.8), so the
move reproduced the ablation's win rather than approximating it. **All 25 test binaries green, zero warnings.**

§2.1 removed the quadratic text materialisation and `pattern_of` was *still* 2245 ms, because the cost was the other
line:

```rust
let declaration = enclosing_class_of(root, binding)?;   // a descent from the ROOT, per declaration
```

`enclosing_class_of` descends from the root to the binding, scanning the children at every level — `O(siblings)` per
level — and it did that **for every declaration in the file**, including the ones it answers `None` for, which is most
of them: a declaration inside a function body still has to be walked to in order to find out there is no class around
it.

This module already documented this exact defect, twice, about the four *type* questions:

> "[they] all answered by *descending from the root to the binding* and keeping what they passed on the way.
> Descending scans a node's children until it finds the one holding the offset, so it is `O(siblings)` at every level"
> — `declarations.rs`, introducing `DeclarationShapes`

**`DeclarationShapes` was the fix and `pattern_of` had never been moved onto it.** What the field wants — the class a
declaration sits in, and that class's written name — is a property of the class and not of the member, so it is
recorded once per class now (`Shape::pattern`, filled in the one pass that already walks the file) and `pattern_of` is
a walk of the ancestor chain that the shape table makes cheap.

**One thing had to be aligned, and finding it was the whole risk of the change:** `enclosing_class_of` recognised
`ClassDef | StructDef | UnionDef`, and `declares_a_shape` listed the first two and **not** `UnionDef` — so a union was
never a shape, and a shape-table lookup would have silently answered "no class" for every member of one. `UnionDef` is
a shape now. That is a behaviour *addition* (a union definition is a declaration like any other), and it is recorded
here because it is exactly the kind of difference that a passing test suite would not have told anyone about: the
tests were green both before and after, and the only reason it was caught is that the two functions' kind lists were
compared by hand before the code was written.


### 3.2 `parameter_list_at` and `enclosing_namespaces_of` — 1576 ms, diagnosed and **not yet done**

Both are the same shape as §3.1 (a walk per declaration for an answer about the file), and one of them is worse than
that. `enclosing_namespaces_of` — asked for every **local** declaration — begins:

```rust
pub fn scope_at(&self, offset: usize) -> Option<ScopeId> {
    self.scopes
        .iter()
        .enumerate()
        .filter(|(_, scope)| scope.range.is_some_and(|range| {
            offset >= range.start_offset && offset <= range.end_offset()
        }))
        // Smallest range wins: the innermost scope containing the offset.
        .min_by_key(|(_, scope)| scope.range.map(|range| range.length))
        .map(|(index, _)| ScopeId(index))
}
```

**A scan of every scope in the file, with a `min_by_key` over the matches — once per local declaration.** That is
`declarations × scopes`, and MSVC headers are mostly inline function bodies, so both factors are large. It is followed
by `scope_chain`, which allocates a `Vec` **and** a `HashSet` per call.

The two fixes, both known and neither done:

```text
parameter_list_at       the `Declarator` it walks up to is already on the `Shape`
                        (`Shape::declarator`, collected in the one pass), so this is §3.1's move again
enclosing_namespaces_of `build_facts` is iterating a scope when it asks this — the caller already knows the answer's
                        scope, and `scope_at`'s scan exists only because the question is phrased as an offset
```

**Why they are not in this round**: both change an *answer* if the substitution is wrong, and the wrongness is silent —
`scope_at` answers "the innermost scope containing this offset", and the loop knows only "the scope whose bindings I am
walking". Those are the same scope for every case anyone has thought of and that is not the same as a proof, and the
test suite was green for §3.1 both before and after a kind-list difference that would have broken every union member.
A change to an answer needs the equivalence argued or the difference measured, and neither was done here.

**Acceptance, unchanged**: `facts` under 400 ms with both done; measured ceiling from the ablation, wall 1634.7 ms.


### 3.3 What this costs, in the units that matter

```text
  today      126 files, 2207 ms wall, 6596 ms CPU  (~52 ms CPU per file)
  §3.1+§3.2  the same 126 files without those two calls: ~766 ms wall, ~2645 ms CPU
  a stub builder, for scale: about 1 ms per file
```

**The remaining gap after 3.1 and 3.2 is the one §4.1 describes** — bodies — and it cannot be closed by making these
two calls faster, only by not having the declarations they are asked about.


---

## 4. The secrets

Verified against source and official documentation; the citations are the point, because the popular summaries of
these tools are mostly wrong.

### 4.1 Nobody builds syntax nodes for function bodies

This is the single most important mechanism, it is stated explicitly by all three, and it is the one we do not have.

```text
clangd      CI.getFrontendOpts().SkipFunctionBodies = true;
            "Skip function bodies when building the preamble to speed up building the preamble and make it smaller."
            clang-tools-extra/clangd/Preamble.cpp

IntelliJ    boolean skipChildProcessingWhenBuildingStubs(ASTNode parent, ASTNode node);
            "Return true if node can't contain stubs… allows speeding up indexing … by reducing the number of the
             AST nodes the platform walks to find all stubbed ones."
            "Use lexer information instead of parsed trees if possible. If impossible, use light AST which doesn't
             create memory-hungry AST nodes inside."
            platform/core-api/src/com/intellij/psi/StubBuilder.java; IntelliJ Platform SDK, "Indexing and PSI Stubs"

Visual      "The C++ Browsing Database Parser is a fuzzy parser that can parse large amounts of code in a short
Studio       amount of time. One reason it's fast is because it skips the content of blocks. For instance, it only
             records the location and parameters of a function, and ignores its contents."
            learn.microsoft.com/en-us/cpp/build/reference/hint-files
```

A function body **cannot contain a declaration any other file can name.** Everything spent on it is spent to answer
questions nobody asks. We build a full lossless tree — every token, every trivia — for every body of every header,
and then walk it to make facts for the locals inside.

### 4.2 The header prefix is parsed once and *serialized*, and loading it is size-independent

clangd's preamble is a real clang **PCH** (`-include-pch`, via `PreprocessorOpts.ImplicitPCHInclude`), and it is
loaded lazily and `mmap`ed:

```text
"The amount of data read in this initial load is independent of the size of the AST file, such that a larger AST
 file does not lead to longer AST load times."
"the cost of using an AST file for a translation unit is proportional to the amount of code actually used from the
 AST file, rather than being proportional to the size of the AST file itself."
                                    clang.llvm.org/docs/PCHInternals.html

"-print-stats on a Hello-World including a large Cocoa PCH:
   20/82685 declarations read (0.024%) | 19/15315 types read (0.124%) | 0/30842 statements"
```

`isPreambleCompatible` then makes reuse **one `stat` per file in the closure** (size + mtime, or an MD5 for memory
buffers), not a re-parse — and `PreamblePatch` keeps serving from a *stale* preamble by splicing only the changed
directives into a synthetic include appended after the PCH, with `#line` markers back to the real positions.

Our equivalent is `TranslationUnitCache`, and the difference is what is stored: a **summary** — derived data that
must be recomputed if anything about the input moves — against **the compiler's own state**, which is re-entered
rather than re-derived. And the key is the content hash of every file in the closure, against a `stat`.

### 4.3 The per-file artifact is self-contained and content-addressed

```text
clangd      the background index is sharded **per source file**, and shards whose digest is unchanged are never
            rewritten (Background.cpp: `FileFilter` → "Skip files that haven't changed")
IntelliJ    "It is critical to ensure that all information stored in the stub tree depends only on the contents of
            the file for which stubs are being built, and does not depend on any external files … Otherwise, the
            stub tree will not be rebuilt when external dependencies change, leading to stale and incorrect data."
Visual      "the parser analyzes the code in each source file in the project and builds a database with information
Studio       about every identifier"
```

We have per-file summaries already. What the others add is the *invariant*: a stub may not depend on another file,
which is what makes its validity a one-file question. **Ours depends on the whole closure** — `SummaryKey` is
`content_hash(text)` + `context_hash(path)`, and the unit cache hashes every file the walk entered — so any change
anywhere in the closure invalidates everything downstream, and a header edit re-indexes a subtree.

### 4.4 The rest, in order of leverage

```text
4.  A cheap structural pass decides what needs the expensive one.
    `computePreambleBounds` is a *lexer* scan, not a parse. CLion has a non-cancellable "Scanning files" phase
    separate from the pausable "Analyzing project". VS re-stats the solution every 60 minutes.
5.  "Small edit before the last #include" does not rebuild anything (clangd's PreamblePatch).
6.  Features declare what they can do on partial data, and degrade instead of blocking.
    IntelliJ's `DumbService` / `DumbAware` / `IndexNotReadyException`: *"all IDE features are restricted to the ones
    that don't require indexes: basic text editing, version control"* — and `CompletionContributor` and `Annotator`
    are dumb-aware, so completion and highlighting keep working. VS: *"This option doesn't disable browsing
    features that rely solely on the database."* VS also defaults to **waiting is off** ("Wait for the browsing
    database to be up-to-date … By default, this option isn't selected").
7.  A hard bound on the working set, with eviction: clangd keeps **3** ASTs (`ASTRetentionPolicy`); VS keeps
    **2–64** translation units, auto-tuned to RAM; both `mmap` the big immutable artifact and nothing else.
8.  Background work at low priority with a priority queue that boosts what the user just opened
    (`ThreadPriority::Low`, `boostRelated`). This does not reduce work; it protects latency.
9.  A prebuilt/shareable index (`clangd-indexer`, `Index.Background: Skip`, JetBrains shared indexes) — how all
    three cope with the fact that the first index of a large project is *hours*.
```

**None of them avoids an O(N) pass over the closure the first time.** The premise "they index instantly" is false;
clangd's own docs say a large project's background index takes *"multiple hours even on very powerful machines"* and
*a few GB* of RAM. What they avoid is (a) building nodes for bodies, and (b) redoing it per keystroke.

---

## 5. What to change here, in order

Each entry names the acceptance, and the instrument already exists for all of them
(`examples/workspace_probe.rs` for the cold profile, `examples/keystroke.rs` for one edit).

```text
1.  Move `pattern_of` onto the shapes table.                     DONE — §3.1, −36% wall, −48% `facts`
    It is the same fix the four type questions already got, for the same reason, in the same file.

2.  Move `parameter_list_at` and `enclosing_namespaces_of`        acceptance: `facts` under 400 ms, wall under
    onto per-file data. §3.2. Measured together at 1576 ms        800 ms for 126 files
    of `facts`, and they are the same shape as item 1: a walk
    per declaration for an answer about the file.

3.  Do not make facts for what a body declares.                   acceptance: fact count for `xstring` drops by the
    Either skip locals in `build_facts`, or — better, and the       share locals are of it, and `facts` drops with it
    same move clangd makes — have the **parse** not build
    subtrees for bodies at all.

4.  Make the parse not build bodies, when nobody is looking.       acceptance: `parse` + `sweep` on a cold index
    A parse mode that consumes a body as a single opaque token      falls by the share of body bytes
    (the lexer already knows where it ends: brace matching at
    token level), used by the indexer and not by the file the
    user is looking at. This is the mechanism in §4.1 and the
    largest single win available.

5.  Key a summary on its own file, and let the closure be a       acceptance: a body edit in one header does not
    separate question. The stub contract in §4.3.                   rebuild any other file's summary

6.  Serialize the closure artifacts so a warm start reads them     acceptance: second run of the same profile is
    instead of recomputing them, and `mmap` them.                   under 200 ms wall, and `StoreStats::reused` is
                                                                    every file

7.  `DumbService`-shaped readiness, in the shell: say per         acceptance: a completion during a cold index
    feature what works on partial data, rather than stalling.       answers, and `isIncomplete` is what tells the
                                                                    client to come back
```

**Item 1 is done and measured; item 2 is the same change again and is worth another ~1576 ms of `facts`. Item 4 is the
architectural one.**

### 5.1 On replacing the syntax tree

Worth answering explicitly, because it is the obvious next thought and the measurements do not support it *yet*.

```text
  parse                     1102.5 ms   24.6% of the CPU     ← the tree is BUILT here
  everything else           3378.6 ms   75.4%               ← the tree is WALKED here
```

Building the tree is a quarter of the cost; **walking it repeatedly is three quarters**, and items 1–2 are both of that
kind. Replacing `rowan` would attack the quarter and leave the rest, at the price of the parser, every consumer, and
the lossless-range property that the whole analysis layer is built on.

What *would* justify touching the tree is item 4 — and it is a change to **what the tree contains**, not to what it is
made of: if a function body becomes one opaque token instead of a subtree, the node count falls by the share of body
bytes, and both the build and every walk get cheaper at once. That is the measurement to take before considering a
different tree library, and it may make the question moot.


