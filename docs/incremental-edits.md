# One keystroke, one file

This document replaces `latency.md`, which was deleted. That one argued about **locks** — who waits for whom — and its
plan was to publish a snapshot so a request would not wait for the indexer. The lock work was done and it worked (the
pump's write hold went from 572 ms to 0–25 ms), and the editor was still slow, because **the wait was never the
problem**. The problem is that one character typed in one file makes this server re-read and re-parse a large
fraction of the program, and that is a design of *what is recomputed*, not of *who holds a lock*.

Every claim below is against this repository, with the file and line that decides it, or against a document linked at
the point it is used. Where a number is quoted from a comment in this codebase, the comment is named. Where nothing
has been measured yet, the document says so.

---

## 1. The claim, and the one number that settles it

Type `x` inside a function body of `main.cpp`. Before the next keystroke can be answered, this server does the
following, and **none of it is optional in the current design**:

```text
   the file's text is scanned, to rebuild its line index                            scan  #1
   the file's text is lexed, to compare its directives                              lex   #1
   the file's text is lexed again, to find what it includes                         lex   #2
   the file is parsed, to rebuild its summary                                       parse #1
   the file is parsed again, with the closure's macro bodies                        parse #2
   the file is lexed again, macro-expanded, rendered, and the rendering parsed      lex #3, parse #3
   every request about it parses it again                                           parse #4
```

and, whenever the answer to "did a directive move" is yes, all of that is preceded by:

```text
   every cached translation unit in the session is thrown away                      (up to 16 of them)
   the unit for the edited file is re-walked over the whole include closure         ~130–400 files
```

**One character, three full parses and three lexes and a line-index scan of the same text, plus a walk of the
include closure.** That is the whole of what the user reported, and it is a property of the design rather than of any
one function.

The rest of this document is: how each of those passes is decided (section 2), what clangd does instead (section 3),
what the IntelliJ platform and Visual Studio do instead (section 4), and what this codebase would have to become
(section 5).

---

## 2. What this codebase does, and where each pass is decided

### 2.1 The notification path

**The server never calls `Session::did_change`.** `didChange` is routed through `Session::did_open` — the handler
does `update_session("didOpen", … session.did_open(&path, &text))`
(`crates/cpp_ls/src/handlers/text_document/text_document_handler.rs:174-176`) — so *every* keystroke takes the
"a document was opened" path. `Session::did_change` exists (`session.rs:1023-1025`) and nothing in the server calls
it. It happens to be harmless today, because both call `buffer_changed` (`session.rs:1012-1014`), and it is worth
writing down anyway: the entry point a reader would look for is not the one that runs.

`buffer_changed` (`session.rs:1144`) does four things, and every one of them is a full-file or whole-project
operation:

```text
  session.rs:1147   directive_signature    lexes the whole new text — directive.rs:414-436, lex at :415
  session.rs:1156   vfs.insert             LineIndex::parse(text), a scan of every character — vfs.rs:131-141
  session.rs:1162   invalidate_dependents  if the directives' *environment* moved (session.rs:1235)
  session.rs:1169   store.forget(path)     drops the summary AND the cooked reading (project.rs:5274-5300)
  session.rs:1176   if moved.layout        units.clear(); units_read.clear()   ← every unit in the session
  session.rs:1180   cooking.want(path)     the file must be cooked again
  session.rs:1182   queue.again(path)      the file must be indexed again
```

(The line-index scan is the one pass here that is genuinely unavoidable per keystroke — a line and column have to map
to an offset — and it is named so that it is not mistaken for one of the removable ones.)

Two of those deserve their own paragraph.

**`units.clear()` on a layout move.** A translation unit is a timeline of `(file, offset)` events
(`summary.rs:1380-1427`), so a directive that shifts position changes it — that is what `layout` means
(`directive.rs:395-403`). The rule is honest and it is applied to the wrong scope: the units dropped are **all
sixteen** (`MAX_UNITS`, `session.rs:4083`), including the ones for files the edit did not touch, and including the
edited file's own — which is the one that is about to be needed.

**`invalidate_dependents`.** For an edit to a *header* whose directives moved, every transitive includer loses its
cooked reading and is re-queued for cooking (`session.rs:1235-1247`). The rule is right — a reading is a reading of
its environment — but on a standard-library header "every transitive includer" is most of the open project.

### 2.2 The three parses and the three lexes

```text
  lex #2     FileIndexer::scan_includes, index/mod.rs:280-304 — lexes the whole file *again* when the summary has
             to be rebuilt, purely to find its `#include` lines (store.rs:498-505). The include scan exists so
             that a header's includes are known before the header is parsed; for a file being *re*-read after an
             edit, the includes are already known from the summary that was just dropped.
  parse #1   FileIndexer::index, index/mod.rs:224 — parses the file's own text, sweeps it, drops the tree
             (index/mod.rs:241-256). Reached by `queue.again` → `index_one` → `store.get`.
  parse #2   SummaryStore::prepare_the_re_read — the "second pass": every file the first pass *parsed* is parsed
             again with the closure's macro bodies, because its scopes could depend on a macro whose body was not
             in the index when it was first read.
  lex #3     render_a_cooked, session.rs:3210
  parse #3   FileIndexer::index_rendering, index/mod.rs:333 — parses the *rendering* of the file, then maps every
             range back into the file (index/mod.rs:344-355). Reached by `Session::cook` → `render_a_cooked`
             (session.rs:3139, 3197).
  parse #4   Session::view_of_the_file (session.rs:1977) — a fresh parse per request, and a parse per declaring
             file for a hover (session.rs:3755). `textDocument/semanticTokens/full` is asked once per edit in the
             file being edited, so this one is on the keystroke path by design.
```

Parse #3 is preceded by lex #3 and by the whole expansion: `render_a_cooked` lexes the text (`session.rs:3210`),
builds `UnitDefinitions` (`session.rs:3214`), builds `FileMacros` from the unit's environment (`session.rs:3215`),
and runs `cook_with(..).render()` (`session.rs:3224`). That is the "macro expansion" the report is about, and it is
a **full-file** expansion and splice.

**Two lexes where one would do.** `directive_signature` and `scan_includes` both lex the whole file, and both run
because the same edit dropped the same summary — the first to compare the directives, the second to re-find the
includes. A single scan that produced both would remove one of them, and neither needs a full lexer: see §5.2.

### 2.3 `unit.definitions()`, inside the loop

`render_a_cooked` calls `unit.definitions()` for **every file it renders** (`session.rs:3214`).
`TranslationUnit::definitions` (`summary.rs:1674`) allocates a `Vec<Option<Arc<MacroDef>>>` of `events.len()` and
builds a `MacroDef` per definable event — over the timeline of the **whole closure**, not of the file being
rendered. On a standard-library closure that is thousands of definitions, rebuilt per file, per slice, per keystroke.

### 2.4 The unit can never be reused for the file being edited

This is the sharpest single finding. A `TranslationUnit` is keyed by the **content of every file the walk entered**:

```text
  summary.rs:1753   TranslationUnit::files()  = every frame        = the root AND the whole closure
  summary.rs:1556   walk_one_file(&root.path, …)                   — frame 0 is the root file itself
  tu_cache.rs:115   put() hashes every one of those files' contents, from the provider
  tu_cache.rs:100   get() re-reads and re-hashes every one of them, and refuses the entry on any difference
```

So the edited file is part of its own cache key. **One character typed anywhere in `main.cpp` changes
`content_hash(main.cpp)`, and the entry for `main.cpp` is refused.** The mechanism the code names as its clangd
analogue — `tu_cache.rs:8-11`: *"The model is clangd's **preamble**: the expensive prefix of a file is built once,
stored, and reused while it still describes the same text"* — is a model with no preamble in it. The key is the whole
file, and the walk it guards is over the whole closure.

The in-memory table (`UnitTable`, `session.rs:4096-4136`) would absorb a body-only edit, since `layout` does not
move — but it holds **16** units with LRU eviction (`session.rs:4111-4126`) while the cooking policy asks for up to
`COOK_ONE_LEVEL_FURTHER = 64` nested headers plus the direct includes plus the root
(`want_the_closure_cooked`, `session.rs:1287-1296`). Sixty-odd roots for sixteen slots: the table thrashes, and the
entry that matters is evicted by the entries that do not.

### 2.5 What the expensive half is worth, measured by this codebase itself

`want_the_closure_cooked`'s own documentation records both halves of the trade
(`session.rs:1249-1296`):

```text
  cooking every file of a 138-file closure      cold 11.7 s | warm 11.9 s   ← a reading is not stored
  what that reading adds                        cook(<string>) reports 1000 declarations,
                                                of which 1 is one the raw reading does not already have
```

and names where the rest comes from:

> The rest — the scopes, the members, the aliases, `std::string` itself — comes from the raw reading once the
> closure's **macro bodies** are in hand (`crate::FileIndexer::with_macro_bodies`).

`FileIndexer::with_macro_bodies` (`index/mod.rs:170`) takes the file's **own text** and reads it with the closure's
macro bodies. It is one parse. The rendering path is lex + expansion + splice + a parse of the result + a mapping
pass. **The codebase has already measured that the most expensive thing on the keystroke path is worth about one
declaration in a thousand, and it is still on the keystroke path.**

---

## 3. What clangd does

From clangd's own `Preamble.h`, which opens with the reason the file exists
([llvm-project/clang-tools-extra/clangd/Preamble.h](https://clang.llvm.org/extra/doxygen/Preamble_8h_source.html)):

> The vast majority of code in a typical translation unit is in the headers included at the top of the file.
>
> The preamble optimization says that we can parse this code once, and reuse the result multiple times. The preamble
> is invalidated by changes to the code in the preamble region, to the compile command, or to files on disk.
>
> **This is the most important optimization in clangd: it allows operations like code-completion to have sub-second
> latency.**

Three mechanisms, and they are the three this codebase is missing.

```text
  1.  the preamble is a *bound* in the buffer
      computePreambleBounds(LangOptions, Buffer, SkipPreambleBuild)
      → the region is knowable from the text alone, before anything is parsed

  2.  reuse is asked as a question, and "stale" is not "unusable"
      isPreambleCompatible(Preamble, Inputs, FileName, CI)
      "Returns true if Preamble is reusable for Inputs. Note that it will return true when some missing headers are
       now available."
      PreamblePatch: "Stores information required to parse a TU using a (possibly stale) Baseline preamble… This
       injected section approximately reflects additions to the preamble in Modified contents, e.g. new include
       directives."
      → an edit *below* the preamble leaves it reusable verbatim; an edit *inside* it can often be patched in

  3.  validating reuse does not re-read the world
      PreambleFileStatusCache: "Cache of FS operations performed when building the preamble. When reusing a
       preamble, this cache can be consumed to save IO."
```

The consequences, in the terms of section 1:

```text
  one keystroke in a body     the preamble is untouched → the PCH is reused verbatim → ONE parse, of one file
  one keystroke in a header   the files that include it are re-parsed against the *same* precompiled prefix
  reuse validation            a stat cache, not a re-read of every file
```

`PreambleData` also keeps what a later parse cannot recover without re-parsing — `Macros` ("Macros defined in the
preamble section of the main file"), `Includes`, `Marks`, and the diagnostics — which is the same lesson as
`TuEvent`: *"As we must avoid re-parsing the preamble, any information that can only be obtained during parsing must
be eagerly captured and stored here."*

That sentence is the design. This codebase captures eagerly (`TranslationUnit` is exactly such a capture) and then
**invalidates the capture on every edit**, so the eagerness buys one use.

---

## 4. What the IntelliJ platform and Visual Studio do

Both answer the same question — *what must be recomputed when one file changes* — and neither answers "the closure".

**The IntelliJ platform** keeps a serialized **stub tree** per file: "a subset of its PSI tree, which contains only
externally visible declarations"
([indexing and stub trees](https://raw.githubusercontent.com/JetBrains/intellij-sdk-docs/3dcf30b956ca96a32db073a22942310681f7e7bd/topics/basics/indexing_and_psi_stubs.md)).
A stub is small, it is **per file**, and it is what every cross-file question is answered from — so no feature ever
parses the file that declares a name. And while the index is being built the platform does not wait: `DumbService`
**restricts features to the ones that do not need indexes**.

Two properties, both about *granularity*: the unit of reuse is **one file**, and the unit of answering is **one
file** — never the program.

**Visual Studio's IntelliSense** keeps the same shape from the other end: a per-file parse is reused, and an edit
re-parses the edited file while the unchanged headers come from a cached, already-processed form. The operative idea
is again per-file reuse keyed on what the file is, not on the program it belongs to.

The three implementations differ in almost everything else — clangd precompiles, IntelliJ serializes stubs, VS caches
parse state — and they agree on one thing, which is the thing section 2 shows this codebase does not do:

> **The unit of reuse is a file, and the thing an edit invalidates is that file.**

---

## 5. The design

Three changes, in the order of how much of the re-parse each removes. Each has an acceptance criterion, and the
criteria are counts of passes, not milliseconds — a count is what the design is about, and milliseconds on this
machine would only re-measure the machine.

### 5.1 A file's preamble is a bound, and the unit is keyed on the bound

**Today.** `TranslationUnit::files()` includes the root (`summary.rs:1753`, `1556`), `TranslationUnitCache::put`
hashes it (`tu_cache.rs:115-130`) and `get` refuses the entry when it moves (`tu_cache.rs:100-105`). The edited
file is in the key of its own unit, so the unit is rebuilt on every keystroke. `buffer_changed` additionally clears
**all sixteen** in-memory units on a layout move (`session.rs:1176`).

**Change.** `preamble_end(text) -> usize`: the offset after the last preprocessing directive that can change the
environment, with clangd's refinement that `#define`s before the first declaration belong to it. Then:

```rust
// What the walk is keyed on, for the ROOT file only. Everything the preamble can reach — the headers it includes
// and everything they include — keeps a whole-text hash, because none of it is being typed into.
//
// The root's own text becomes `(preamble_end, hash of text[..preamble_end])`. A character typed below that bound
// cannot change the environment at any offset above it, which is the whole of what the timeline is for.
```

and the invalidation rule in `buffer_changed` becomes "the preamble moved", not "a directive moved":

```text
  today    moved.layout → units.clear()                    every unit in the session
  change   preamble moved (bound or bytes) → drop THAT file's unit   the one file that changed
```

**The one hard part, and it must be written down rather than discovered.** A file's own `#define`s *below* the
preamble are still events in its frame of the timeline, and their offsets may have moved. clangd handles exactly
this with `PreamblePatch` — the main file is parsed against a *possibly stale* preamble with the delta injected.
Here the same shape is needed: reuse the cached unit, and re-derive **the root frame's events after
`preamble_end`** from the new text. For a body edit those events are usually none; the machinery has to exist for
the ones that are not.

**Acceptance.** Type one character in a body, in a file whose closure is more than a hundred files: the `Walk` and
`Closure` stages of `crate::stages` gain **zero entries**, and the unit for the edited file is the same allocation
before and after (a pointer comparison in a probe is enough).

### 5.2 The file being edited is parsed once, not four times

**Today.** lex #1 (`directive.rs:415`), lex #2 (`index/mod.rs:281`), parse #1 (`index/mod.rs:242`), parse #2 (the
second pass), then lex #3 + expand + render + parse-the-rendering + map (`session.rs:3210-3229`), plus parse #4 per
request (`session.rs:1977`).

**Change, in four parts, each independently shippable:**

1. **One scan, not two lexes.** `directive_signature` (`directive.rs:414-436`) and `scan_includes`
   (`index/mod.rs:280-304`) both lex the whole file, for the same edit, one after the other. Neither needs a lexer:
   what both want is the lines that begin with `#`, and what `directive_signature` additionally wants is each
   directive's own text and offset. One line-oriented scan producing `(directive text, start offset, the `#include`
   spelling)` answers both, and removes lex #1 **and** lex #2.
2. **The edited file must not go through the second pass.** The second pass exists because a file first parsed
   before its includes were in the index may have wrong scopes. For the file being edited, the unit is already in
   hand, so its `environment_of(path)` is too — and `FileIndexer::with_macro_bodies` is the call that reads a file
   correctly with an environment. Parse it once, with the environment.
3. **The edited file must not be rendered at all, unless a question needs the rendering.** The codebase has measured
   what the rendering buys: **one declaration in a thousand** (`session.rs:1262-1265`), over a `raw + macro bodies`
   reading that costs one parse. So:
   ```text
     raw parse with macro bodies      ← what the index holds for the edited file, per keystroke
     the rendering                    ← built on demand, when a view or a macro question asks
   ```
   This removes lex #3, the expansion, the render, the render-parse and the mapping pass from the keystroke.
   **The policy already exists** — `Session::view` (`session.rs:2105-2129`) answers from a cached rendering when
   `(path, content_hash(text))` matches, and otherwise parses the file's own tokens and enqueues
   `macro_work.want(path)` for the pump to build the rendering later. The cook is the one caller that does not
   follow it.

**Acceptance.** One keystroke in a body: the `Parse` stage gains **one** entry for that file. Reported by a probe
that reads `StageTimes` before and after a `did_change` + drain.

### 5.3 The hot set stays hot, and the walk is not re-validated by re-reading

**Today.** `want_the_closure_cooked` marks up to `COOK_ONE_LEVEL_FURTHER = 64` nested headers per drain
(`session.rs:1287-1296`), each of which is a lex + render + parse; the unit table holds 16 and thrashes
(`session.rs:4083`, `4111`); and `TranslationUnitCache::get` re-reads and re-hashes every file of the closure on
every miss (`tu_cache.rs:100-105`), which the code measures at "13 MB of text hashed in ~60 ms" (`tu_cache.rs:27`)
— and it is paid *again for each root*, so a project-wide cook pays it dozens of times.

**Change.**

```text
  cooking        what a request *named* (Session::want_cooked_reading), not a fixed neighbourhood per drain
  the table      the open files' units are pinned; the LRU is for the rest
  validation     (size, mtime) first, content hash as the fallback — clangd's PreambleFileStatusCache,
                 and it is the same reason: a check that costs a read is a check that cannot be paid per query
```

**Acceptance.** Open one file, type, drain: the number of `Render` stage entries is **one** (the file that was
typed into) and not sixty-five.

---

## 6. Order, and what each stage is judged by

```text
  1.  the preamble bound, and a unit keyed on it
      judged by: zero `Walk`/`Closure` entries for a body edit in a >100-file closure

  2.  one parse of the edited file
      judged by: the `Parse` stage's entry count for one keystroke is 1, and the rendering is built on demand

  3.  the hot set, and a cheap validity check
      judged by: `Render` entries per drain is 1, not 65

  4.  the per-request parse (Session::view_of_the_file)
      judged by: a query's `Parse` entries, once 1–3 have made the file's own parse cheap
```

**Stage 1 is first** because it is the one that removes a *walk of the closure* from the keystroke, and because it
is small: a bound function, a key that uses it, and an invalidation rule that names one file instead of sixteen.
Stage 2 is second because it removes three passes from the file the user is typing into. Stage 3 is third because
it bounds how much *other* work a keystroke can set off, which only matters once the other two are cheap.

**Stage 4 is deliberately last.** The per-request parse is real (a cursor query needs a tree), and it is the one
stage whose fix is not obvious: a session cannot keep a tree per file without deciding what evicts it. Stages 1–3 do
not depend on it.

---

## 7. What this document is not

It is not a plan to make the *index* faster. Everything here is about what a keystroke causes to be recomputed. The
background index may take as long as it takes: clangd's own is documented as running for hours on a project the size
of Chromium, and no request waits for it, because a request never asks for the program — it asks for a file.

It is not a claim that the lock work was wasted either. It removed a real wait, and it put a detector on the read
path (`AnalysisState::run_blocking` now reports permit / gate / session / ran apart) that is what made this document
possible: without it, "the completion is slow" could not be told apart from "the completion is queued".

And it is not an argument that the rendering is worthless. It is worth **one declaration in a thousand**
(`session.rs:1262`), which is exactly why it should not be built on the path a keystroke takes. Move it to where a
question needs it and it costs nothing at all.

### What is not yet measured, and must be before stage 1 is believed

```text
  how many files a body edit currently re-parses        the passes above are counted from the call graph,
                                                        not from a run — a probe must count them
  how long one closure walk is on this machine          the code's numbers (1003 ms for a unit walk, 572 ms for a
                                                        drain's write hold) came from a slower machine
  how often `moved.layout` is actually true             the argument assumes body typing does not move it, which is
                                                        true for a file whose directives are all at the top and
                                                        must be checked on a real one
```

### Two things the audit found that are not this document's subject, and are not obviously right

Both are in `ProjectIndex` and both are about an invalidation that the *insert* path does but the *forget* path does
not. Neither is proven to produce a wrong answer, and neither is proven not to:

```text
  visibility_answers   cleared by `insert` (project.rs:5176-5180) and `insert_cooked` (:5351-5354),
                       not by `forget` (:5274-5300) and not by `forget_cooked` (:5390-5396).
                       The memo is a *condition evaluated against macros*, and a file leaving the index can
                       change an answer as surely as a file joining it.
  module_interfaces    cleaned by `insert_at` (:5247-5257), not by `forget`.
                       A module whose interface unit was forgotten would keep being named.
```

They are written down here because they are the same shape as the defect this codebase has already paid for twice
(`status.md` §2): **one question, more than one place that has to answer it.** Whoever takes the invalidation work
in §5.1 is the person to check them, because §5.1 changes what invalidation is allowed to throw away.

---

## 8. Where this got to

### 8.1 Stage 1 is implemented, and its acceptance passes

```text
  RootKey                     src/tu_cache.rs — WholeText | Preamble { end, hash }, and `hash_of` as the one place
                              that says what a key requires of a text
  preamble_end                src/preprocess/directive.rs — a third field on DirectiveSignature, taken from the
                              scan that already ran, so nothing new is computed per keystroke
  the key the session passes  Session::root_key_of — the file's preamble when its text is in reach
  the invalidation rule       Session::buffer_changed drops **that file's** unit, not all sixteen
  the instrument              Session::unit_stats() -> UnitStats { from_memory, from_disk, walked }
```

```text
  cargo test --release -p cpp_code_analysis --lib -- typing_in_a_body_does_not_cost_the_walk
      test session::tests::typing_in_a_body_does_not_cost_the_walk_a_translation_unit_is ... ok
```

The criterion is §5.1's, as a **count**: one session walks three units and writes them; a session that has never seen
the file reports `walked: 0` both for the text that was walked **and for that text with a character typed into its
body**; a session handed the same file with an `#include` added above the body reports `walked > 0`. See
`UnitStats` for why a count and not a duration.

### 8.2 What the work turned up on the way, and it was larger than the stage

`cargo test -p cpp_code_analysis` had not been runnable for a round, for two reasons that hid each other:

```text
  1.  the lib test target did not compile — 4 errors: three `update` calls without the `label` the signature had
      gained, a `Name` literal without `modifiers`, four assertions calling `units.is_empty()` on a `Mutex`

  2.  once it compiled it did not finish: `Session::index_everything` never returned. It looped on
      `pending_work()` — which counts the **cooking backlog** — and only ever called `advance`, which *marks* files
      for cooking and drains nothing. The marking is idempotent, so the backlog sat at a fixed size for ever, and
      every `session::tests` entry hung for sixty seconds and then for ever. The suite was killed rather than run,
      which is what "the tests are too slow" turned out to mean.
```

Both are fixed; the suite finishes in **0.43 s**. Five of its tests fail, and they fail in every configuration tried —
including one with the whole of this stage absent — so they are not this stage's; they are listed with the two
experiments that attributed them in `status.md` §6, together with one handshake test that is a coin rather than a
failure and has not been attributed at all.

**The lesson is this document's own, in different clothes.** §2 is about work whose unit is the program rather than
the file. The test loop had the same shape: it looped on a count that included a kind of work it never did. A bound
that cannot be reached and a loop that cannot terminate are one bug wearing two hats, and both stayed invisible
because the thing that would have shown them — running the suite — was the thing that was broken.

