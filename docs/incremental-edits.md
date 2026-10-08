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

### 5.2 The file being edited is parsed once, and not rendered

**Measured first, because the plan above was written from the call graph and the call graph was wrong.**

```text
  cargo run --release -p cpp_code_analysis --example keystroke
```

```text
  --- a character in a body ---
    include-scan 1, parse 1, sweep 1, lex 1, render 1, render-parse 1, render-sweep 1, map 1
    …units  memory +3, disk +0, WALKED +0
    => parsed 1 time(s), lexed 1, rendered 1, rendering parsed 1
```

Three things follow, and the first two contradict what this section used to say.

```text
  1.  there is no second parse to remove. The second pass re-reads a file only when its scope walk could depend on
      a macro body (`mentions_one_of`) or when the unit walk decided about its `#define`s — and a `.cpp` that
      invokes no namespace-opening macro is in neither set. `parse` is **1** already, for this shape and for the
      `#define`-below-the-body edit beside it. Item 2 of the old plan would have bought nothing.

  2.  the closure walk is gone. `WALKED +0` for a body edit is stage 1 working; the same counter moves for the
      two edits that really are inputs to the file's timeline (a `#define` below the body, an `#include` above
      it), which is what says the rule is a rule and not a hole.

  3.  **the render is the whole of what is left.** One character costs a lex, a macro-table build, a full
      expansion and splice of the file, a parse of the result, a sweep of that parse and a pass mapping every
      range back — six stages over the file's tokens, on top of the raw parse and sweep. §1 counted it as one of
      three parses; it is in fact the *only* one that the edit does not need.
```

So the change is the one §5.2 always named and never justified: **the edited file must not be rendered unless a
question asks for the rendering.** And the measurement changes why it is hard:

> The raw reading has to be good enough to answer without the rendering — which means it has to have been built
> **with the closure's macro bodies**. Today it is not: the raw parse of the edited file is made without an
> environment, and the second pass repairs it *only for files the filter selects*.

That is the item that was missing, and it is the enabling step rather than a saving of its own:

```text
  1.  SummaryStore::prepare_with_the_environment(path, bodies)
      the same body as `prepare`, plus `FileIndexer::with_macro_bodies` — one parse, from the text and the
      environment the session already holds for this file

  2.  Session::index_one uses it when a unit is *already held* for the path
      `held_units().get`, never a walk: a unit that is not in memory is one the closure has not been read for,
      and an index step must not be where that happens. A miss is today's path, unchanged.

  3.  the file is then marked as built-with-its-environment, so the second pass has nothing to repair
      (`Step` carries the fact; `finish_a_slice` leaves those files out of `parsed_since_the_last_pass`)

  4.  and only then can the render be skipped — because now the raw reading answers with the same scopes the
      rendering would have given it
```

**Steps 1–3 buy no passes on their own** — measured, there are none to buy. They exist so that step 4 is a change
of *policy* with the quality already in hand, rather than a change of policy that trades quality for speed.

### Steps 1–3 are done. Step 4 is blocked, and the reason is a defect, not a plan

```text
  SummaryStore::prepare_with_the_environment(path, bodies, facts)   src/index/store.rs
  FileIndexer::with_macro_body_readers / with_a_macro_environment   src/index/mod.rs
  Session::index_one reads a file with the environment it holds     src/session.rs — held units only, never a walk
  Step::with_the_environment                                        src/index/worklist.rs
  …and `finish_a_slice` leaves those files out of `parsed_since_the_last_pass`
```

Verified by `a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass`: with no unit in memory the
file goes to the second pass exactly as before; with one, it does not. The probe is unchanged — which is the point,
because these steps are not where the passes are.

**And step 4 must not be taken yet.** While tightening that test it came out that the sharper assertion cannot be
written, because the thing step 4 assumes is *not currently true*:

```text
  fixture      ns.h:  #define BEGIN_NS namespace one {
               api.h: #include "ns.h"   BEGIN_NS struct Widget { int size; }; END_NS

  after a full index_everything:
      api.h's RAW reading      ["Widget", "Widget::size"]     scope = None
      api.h's COOKED reading   ["Widget", "Widget::size"]     scope = None   ← the rendering did not expand it
      definition("one::Widget", api.h)   Unknown(NotDeclaredHere("one::Widget"))
      definition("Widget", api.h)        Yes(… scope: None …)
```

**The rendering does not place the macro's scope.** That is not a new defect and not this document's subject: it is
one of the five failures `status.md` §6 records, and `editing_a_header_invalidates_the_readings_that_depend_on_it`
fails through the same fixture and the same name (`two::Widget` there, `one::Widget` here).

It matters here because it removes the ground step 4 stands on. Step 4 is justified by *"the rendering is worth one
declaration in a thousand over `raw + macro bodies`"* (`session.rs:1262`) — and a measurement of what the two
readings differ by cannot be made while one of them is not doing its job. Skipping the render now would trade a
partially working capability for speed and make the resulting loss **indistinguishable from the bug that is already
there**, which is the worst of both: no speed measurement anyone can trust, and a new failure with an old name.

So the order changes again, and for the third time it is a measurement that changed it:

```text
  done     the three failures about a cooked reading that does not carry what it should
           they were two cache-invalidation bugs, one layer apart — §8.5 and §8.6 — and neither was about the cook
  done     steps 1–3 above — which were **dead code** until §8.7 wired them to the path that actually runs
  done     the measurement step 4 was waiting on — and it says step 4 is the wrong change. See §8.7.
  now      stage 3, stage 4
```

**What step 4 would have had to keep, and it is a list rather than a hope.** §8.7 is why it was not taken, and this
is the list that would have had to be measured first:

```text
  cooked_declarations(path)          the queries that read a file through the index. They fall back to the raw
                                     summary — which is the whole point — and `cook(<string>)`'s "one declaration in
                                     a thousand" (session.rs:1262) is the measurement that says how much that costs
  diagnostics from the rendering      `Session::diagnostics` answers from the cooked reading *if it is held* and
                                     from the file's own text otherwise. The rendering's parse errors are richer than
                                     the raw parse's, and that difference is visible to a user
  every other file                   unchanged: the direct includes and the level under them are still cooked, which
                                     is what `<string>`/`<xstring>` needs
```

Steps 1–3 are safe and independently verifiable; step 4 is a behaviour change and should ship behind the ability to
measure that list. Steps 1–3 and the scan are **done** — and the scan is item 1 of the old plan:

**Done: item 1 of the old plan — one scan, not two.** `SummaryStore::prepare` no longer runs the early include scan
(`SummaryStore::prepare_inner`, with the scan optional), because the scan exists to feed a *wave*'s frontier
(`prepare_closure`) and a caller preparing one file has no frontier. The parse resolves the same `#include` lines a
moment later; what is saved is a full lex of the edited file on the `get`/`catch_up` path. The probe still shows
`include-scan 1` for an edit driven through `index_everything`, because that path *is* a wave — which is the rule
working as intended rather than a hole in it.

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
  1.  the preamble bound, and a unit keyed on it                                    DONE
      judged by: zero `Walk` entries for a body edit        — and measured: `WALKED +0`, with the two edits that
                                                              are inputs to the timeline still walking

  2.  one parse of the edited file, and no rendering of it                          PARTLY
      judged by: the probe's line reads "parsed 1, rendered 0"
      done:      the early include scan off the one-file path (`SummaryStore::prepare_inner`)
      measured:  there is **no second parse to remove** — `parsed 1` already — so the plan's item 2 was empty and
                 the render is the whole of what remains
      left:      `prepare_with_the_environment` (steps 1–3 of §5.2) and then the policy change (step 4)

  3.  the hot set, and a cheap validity check
      judged by: `Render` entries per drain is 1, not 65

  4.  the per-request parse (Session::view_of_the_file)
      judged by: a query's `Parse` entries, once 1–3 have made the file's own parse cheap
```

**Revised order, and the reason.** Stage 2 splits: the render cannot be skipped until the raw reading is
environment-correct (§5.2 steps 1–3), and those steps buy nothing on their own. So the honest order is **1 (done) →
the enabling steps of 2 → the policy change of 2 → 3 → 4**, with the probe as the instrument at every step, and
`Session::unit_stats` beside it for the walk.

**Stage 4 is still last.** The per-request parse is real (a cursor query needs a tree), and it is the one stage
whose fix is not obvious: a session cannot keep a tree per file without deciding what evicts it.

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
  how many files a body edit currently re-parses        MEASURED — examples/keystroke.rs. One parse, one lex, one
                                                        render, one render-parse; no walk. §5.2 has the table,
                                                        and it falsified one of the plan's own items.
  how long one closure walk is on this machine          still open. The code's numbers (1003 ms for a unit walk,
                                                        572 ms for a drain's write hold) came from a slower
                                                        machine, and the probe's fixture is four small files — its
                                                        `unit-get` of 1.5 ms is not a number about a real closure.
  how often `moved.layout` is actually true             still open. The probe shows it does move for a `#define`
                                                        below the body and for an `#include` above it, which are
                                                        both inputs to the timeline — but a real file, with its
                                                        directives interleaved through its body, is the case the
                                                        rule has to survive.
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

### 8.3 Stage 2: the plan's second item was empty, and the third is the whole of it

The probe (§5.2) says one character in a body costs `parsed 1, lexed 1, rendered 1, rendering parsed 1, WALKED +0`.
Two of the three things §1 listed as the cost of a keystroke were already gone or never existed:

```text
  the closure walk        gone, and that is stage 1 — `WALKED +0`, with the two edits that really are inputs to
                          the file's timeline still walking
  the second parse        never happened for this shape. The second pass re-reads a file only when its scopes could
                          depend on a macro body, and a `.cpp` that invokes no namespace-opening macro is not in
                          that set. The plan's item 2 would have bought nothing.
  the render              the whole of what is left: lex, macro table, expansion, splice, parse of the rendering,
                          sweep, and the map back
```

**The plan was written from the call graph and the call graph was wrong about which of the three mattered.** It cost
one probe to find out, and the probe is now in the repository
(`cargo run --release -p cpp_code_analysis --example keystroke`) with the acceptance line the section asks for. That
is the whole argument for §7's first rule, one more time: the count is cheap, the rewrite is not.

**What is done of stage 2:** the early include scan is off the one-file path (`SummaryStore::prepare_inner`), which
is a full lex of the edited file that the `get`/`catch_up` path was paying for a frontier it does not have. The
probe still shows `include-scan 1` when the edit goes through `index_everything`, because that path *is* a wave and
the scan is what feeds it — the rule working, not a hole.

**What is not:** §5.2's step 4, the policy change that stops the render. It was blocked on the three cooked-reading
failures and **is no longer**: §8.5 and §8.6 are both fixed, `cargo test -p cpp_code_analysis` is green for the first
time, and the measurement step 4 needs — what the rendering buys over `raw + macro bodies` — can now be taken on a
suite that is not lying about its baseline.

### 8.4 Stage 2's enabling steps, and what they cost to verify

```text
  done   SummaryStore::prepare_with_the_environment   one parse, from the text and the environment a caller holds
  done   FileIndexer::with_macro_body_readers         the same evidence as two trait objects, for a caller that has
                                                      already erased the type (a `dyn MacroFacts` is not a
                                                      `MacroFacts`, which is why the existing setter is generic)
  done   Session::index_one                           uses it when a unit is **already held** — `held_units().get`,
                                                      never a walk: an index step is not where a closure is read
  done   Step::with_the_environment                   and `finish_a_slice` leaves those files out of the second pass
```

The verification is worth more than the change. Three attempts were needed, and the two failures are the kind this
document keeps finding:

```text
  1.  `macro_readings` as the witness            WRONG. It records the bodies the **plain shape reader cannot
                                                 settle**, and `#define BEGIN_NS namespace one {` is settled
                                                 without any environment — so a fixture with an easy macro would
                                                 have passed a test about a mechanism that never ran.
  2.  `environment_of(path)` with the session's  WRONG, and silently: a unit's frames are keyed by the spelling the
      own spelling of the path                    *walk* used, so the lookup answered `None` and the whole feature
                                                 was a no-op that compiled. `Session::units_for_the_pass` records
                                                 the same trap from the other side; the fix is to ask with
                                                 `index.summary(path).path`.
  3.  the scope, `definition("one::Widget")`     RIGHT, and it fails — for a reason that is not this change. See
                                                 §5.2: the cooked reading does not place the scope either.
```

The third is the one that changed the plan. It also produced the test that is in the repository, which asserts the
thing the change is about — **which files the second pass is handed** — and carries the note to tighten it into the
scope assertion when the cook defect is fixed. A test asserting the scope today would be red for a reason with
nothing to do with the flag, which is `status.md` §6's second rule ("a criterion that bites may still bite the wrong
shape") arriving from a new direction.

**And the second failure is the one to remember.** A `HashMap` keyed by the walk's spelling, asked with the
session's, is not a compile error and not a panic — it is a feature that is silently absent. It was found only
because a test asserted the *effect* rather than the *call*.

### 8.5 The first of the three cooked-reading failures was a cache-soundness bug

Tightening the test above needed a fixture where the environment *matters*, and building it found the defect that
`status.md` §6 had been listing as "a cooked reading that does not carry what it should" without a cause:

```text
  fixture   ns.h:  #define BEGIN_NS namespace one {
            api.h: #include "ns.h"   BEGIN_NS struct Widget { int size; }; END_NS

  a session that reads ONE step    indexes api.h, queues ns.h, and walks api.h while ns.h is not yet in the index
                                   → WALK "/p/api.h" frames=["/p/api.h"]      a closure of one, from a closure of two
  …and writes that timeline        the entry's recorded closure is the one frame it *entered*, so its key cannot
                                   tell a short closure from a complete one
  every later session              served it
  the environment it answers       without `namespace one {`
  so the reading of the file       `struct Widget` at file scope, `one::Widget` NotDeclaredHere
```

**The walk was never wrong.** A fresh walk over the same inputs produced both frames throughout — which is what the
probe showed, and what made the diagnosis a measurement rather than a reading:

```text
  cached unit frames = ["/p/api.h"]              what the session was serving
  fresh walk frames  = ["/p/api.h", "/p/ns.h"]   the same inputs, walked now
  …and with the offending session removed altogether:  definition("one::Widget") → Yes(… scope: Some("one") …)
```

#### The two meanings of a file the closure does not hold

`TranslationUnit::walk` stops at a file its closure cannot offer, and its early return says why that is right: a
system header nobody indexed is **outside the analysis**, and treating it as "may define anything" was measured to
collapse the corpus's conditional evidence from millions of facts to tens of thousands. That reading is permanent.

While an index is running the same absence means **not yet** — the file is in the queue and will be read — and a
timeline built without it is a timeline of a *smaller program*. Nothing in the timeline says which of the two it is,
so caching it asserts the wrong one. The distinction is invisible to the walk and visible to the **session**, because
the queue is the session's:

```rust
// session.rs
fn closure_is_still_growing(&self, closure, in_closure) -> bool {
    // a target that is missing **and queued** is a target that is coming
}
```

#### The fix

```text
  HeldUnit                    a held timeline carries `provisional`
  translation_unit_of         a provisional timeline is used for this run, and dropped as soon as the queue drains
  put                          a provisional timeline is **never written to the cache**
  unit_and_state_of           and `cooking_materials` refuses one outright
```

The last line is a rule of its own and it is the same defect one layer up: **a rendering sticks.** A timeline is
re-walked the moment the queue drains, but a cooked reading is filed in the index and nothing re-cooks a file that has
one — so a rendering made from an environment missing whatever the queue had not read stays wrong for the life of the
session. Leaving the file uncooked keeps it *wanted* (`want_the_closure_cooked` asks for every open file without a
reading), so a later drain renders it when the timeline is the whole program.

**That last rule is not verified by a test.** The suite is identical with and without it. It is in because the
argument is the same one that the measurement above settled for timelines, and it is written down here as unverified
rather than as done.

#### What this changed, measured

```text
  lib suite        589 passed / 5 failed  →  590 passed / 4 failed
                   `a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file` passes
  the other two of the three cooked-reading failures stay red, and are now one **path** rather than a class:
                   after `ns.h`'s macro changes from `namespace one` to `namespace two`, the reading is not rebuilt
                   around the new body. The unchanged form of the same fixture works.
  the probe        unchanged — the fix is about correctness, and it buys no passes
```

#### And one mistake worth keeping

The fix's first version **hung the suite**, and the reason is a rule about Rust that a compiler cannot report:

```rust
if let Some((provisional, unit)) = self.held_units().get(&key) {   // a MutexGuard temporary
    …
    self.held_units().remove(&key);                                // ← re-entrant lock, one thread, deadlock
}
```

A `MutexGuard` created in an `if let` **scrutinee** lives until the whole statement ends. Binding the result to a `let`
first is the fix. It cost a per-step trace to find, because the first guess — that the loop was in
`Session::index_everything` — was wrong, and the trace that would have said so printed nothing at all.

### 8.6 The other two were the same bug one layer out: a stale timeline, not a stale reading

`two::Widget` came back `NotDeclaredHere` after `ns.h`'s macro changed from `namespace one` to `namespace two`, and
every step of the invalidation was *working*: the test's own preceding assertion (both `api.h` and `other.cpp` lose
their cooked readings) passed. So the reading was dropped correctly and then rebuilt wrongly.

```text
  a cooked reading is built from a translation unit's environment (`cooking_materials` → `environment_of`)
  the unit cache **on disk** is keyed on the content of its closure, so an edit refuses it on its own
  the unit cache **in memory** is checked against nothing at all
  …so `invalidate_dependents` dropped the reading, kept the timeline, and the file was re-cooked out of a timeline
  that still said `namespace one`
```

**The disk cache looked invalidation-proof, and the table in front of it was not.** That is why this hid for so long
in the same place as §8.5: both are questions about the *validity* of a cached answer, asked one layer apart, and
neither shows up as a parse error, a panic, or a wrong-looking tree.

The fix is one line beside the one that was already there:

```rust
// Session::invalidate_dependents — the set was already being walked; it just was not being cleared
self.held_units().remove(&queue_key(&dependent));
self.store.index_mut().forget_cooked(&dependent);
```

#### What this one changed, measured

```text
  lib suite        590 passed / 4 failed  →  594 passed / 0 failed
                   the first time this suite has been green, and it cost no new behaviour: both fixes are about
                   refusing to serve an answer that was made about a program that has since moved
  the probe        unchanged — `parsed 1, lexed 1, rendered 1, WALKED +0`, as every correctness fix in this section is
```

#### And four of the five were tests, which is the part to be careful about

Re-basing a failing test is the one edit that has to be argued rather than made, so the argument is written down in
`status.md` §6 per group. In short: three assertions were claims about the **renderer's whitespace**
(`text.contains("= 4 ;")`, `find(" 4 ")` twice) and were red while the expansion each is named after was visibly
present; two more asserted the cooking policy from **before** `COOK_ONE_LEVEL_FURTHER`, whose own comment records why
the level under a direct include is reached on purpose (`<string>` → `<xstring>`). The whitespace ones are re-based on
the digit and *strengthened* (the stitched test now also asserts neither macro's name survived); the policy ones gained
a fourth fixture file so they pin **level 2 cooked, level 3 not**, distinguishing one more level than the versions that
used to pass.

### 8.7 Steps 1–3 were dead, and the measurement that made step 4 unnecessary

#### The wiring bug: a mechanism that was correct, tested, and never reached

§5.2's steps 1–3 put the environment into `index_one`'s **`None`** arm — the arm that runs when a file was not
prepared by a wave. `Session::advance` prepares a wave whenever it has more than one step to take, and the pump asks
for sixteen. So on every path a running server takes, `index_one` was reached with `Some`, the summary came from the
wave, and the environment was never consulted:

```text
  the flag `Step::with_the_environment`   never once true on a real edit
  the test that covers it                 passed, because it called `advance(1)`
```

That is the shape `status.md` §6's second rule describes — a criterion that bites the wrong shape — and it cost one
line to fix, after being found by asking where the flag is set rather than whether the code is right:

```rust
// Session::advance — a file whose environment this session holds is left out of the wave
let holds_the_environment = self.holds_an_environment_for(&path);
if left > 1 && !holds_the_environment && !self.prefetched.contains_key(&key) { … }
```

**Leaving it out** rather than discarding its answer afterwards, and the difference is not cosmetic: the wave writes
what it builds to the disk cache, so a session that threw the wave's answer away would then be served that entry as a
**cache hit** — measured, the step reported `Reused` where the test expected `Built`, and the environment went unused
anyway. Two tests caught it. The cost of the fix is one parse on the writer instead of on the readers, paid for one
file: the one whose timeline is in hand, which is the one being typed into.

#### The measurement

```text
  cargo run --release -p cpp_code_analysis --example cook_value -- target\cook-value\main.cpp
```

A translation unit including `<string>`, `<vector>` and `<map>` — 11 files cooked — then **typed into**, then measured
per file as a set difference of qualified names:

```text
  file       raw   cooked   only cooked   only raw
  xstring    163      511            353          5
  vector     118      332            262         48
  map        107      107              4          4
  string      45       42              1          4
  main.cpp    12       12              0          0   ← the file being edited
```

**For the file being edited the rendering adds exactly nothing**, which is the number step 4 was waiting for: 353 for
`xstring`, 262 for `<vector>`, and **0** for the translation unit a person is typing into.

#### …and the same measurement dissolves step 4

Step 4 was written as *"the edited file must not be rendered at all, unless a question needs the rendering"*, on the
model that the cook is eager and the question is rare. Two facts about where a cooked reading of the edited file
actually comes from say otherwise, and both are one grep:

```text
  want_the_closure_cooked(root)      marks the **direct includes and the level under them** — it has never marked
                                     the root. So the eager path does not cook the edited file.
  cpp_ls/src/context/analysis_state.rs:167-174
                                     "**A file a request is about gets its cooked reading** — and this is the only
                                     place that knows which file a request named." Every request that names the file
                                     asks for it, and the comment says why: `isIncomplete` is the client's signal to
                                     come back for the compiler's reading.
```

So the rendering of the edited file is **request-driven already**. Removing the eager cook would not remove the work;
it would move it from a background drain to the request that is waiting, which is the wrong direction. And making the
request *stop* asking means changing `want_cooked_reading`/`is_ready_for_a_request_about` — which is not a latency
change at all but a decision about what a client is promised, and the one capability it puts at risk is the one this
document's own probe explicitly does not measure:

> **Diagnostics.** `Session::diagnostics` answers from the cooked reading when one is held and from the file's own
> text otherwise, and the two differ exactly where a branch nobody takes has an error in it. A client told
> `isIncomplete` never becomes false for a file that will never be rendered.

**So the honest end of stage 2 is here**: the scan is gone from the one-file path, the edited file is parsed once with
its environment instead of twice, the timeline survives a body edit, and the render is measured to be worth nothing
for the file being typed into — while the thing that decides whether to render it turns out to live in the shell, one
layer above this document's subject, and to be a promise rather than a cost.

What is left for latency is **stage 3** (the hot set: `Render` entries per drain is 1, not 65) and **stage 4** (the
per-request parse in `Session::view_of_the_file`), and neither depends on this.





