# Refinement — what it is, when it runs, and what it costs **now**

The question this answers: refinement is lazy, but it is expensive, and every kind of request may be waiting on it. Is
a file refined once? What exactly is the difference between the coarse reading and the refined one? And with "low
latency, low memory" as the goal, where is the room?

Everything below is measured on this checkout (`22e5338` + the two commits after it), with the probes named beside each
number. The design this belongs to is `docs/indexing-design.md` (L1–L4, phases 0–3); this file is the state of the
implementation.

---

## 1. The four layers, as the code has them

```text
L1  FileSummary of the file's own text        env_hash == 0        the first pass, and every start
L2  the same file as its closure renders it   env_hash == closure  a "refinement": macros expanded, branches taken
L3  the rendering's parse tree                 —                   Session::view (cached, 8 entries / 64 MB)
L4  the text                                   —                   the VFS, shared with the client's buffer
```

**What L1 has, and what it cannot see.** The first pass parses the file's own bytes with an **empty** macro
environment, and records facts from the tokens: names, kinds, the scope *path* written literally, signatures, macro
names with their spelling-level bodies, include edges, and each declaration's `guard`. It cannot see:

* a scope **opened by a macro body** — `_STD_BEGIN` is a word, so `struct Widget` lands at file scope instead of
  `std::Widget`;
* a declaration **written inside a macro argument** (`UPROPERTY(…) int x;` keeps `x`, loses the annotation);
* which side of an `#if` the program takes — both declarations are in the text, so both are facts, each marked
  `split_by_a_directive`/`guard`.

**What L2 is.** The file **rendered through its closure**: the preprocessor resolves includes and expands macros, so
the scope opened by the body is literal text (`namespace std {`), the taken branch is the only one present, and
declarations inside macro arguments are ordinary declarations. L2 is what a compiler's parser is handed, which is why
it is the reading every semantic answer wants.

**What changed in phase 2.** L2 used to be *a second parse of the file with a timeline (translation unit) in hand*.
Phase 2 stopped building timelines for the pass: the reading is now **composed from the index's macro facts**
(`SummaryStore::prepare_the_re_read` and the vocabulary in `cpp_parser::BodyShape`), and the session refines by
**rendering** the file (`prepare_the_re_read` with `walk_its_own = true`, then `render_them`). One consequence matters
below: the refinement of a file is no longer a summary the disk cache can hold — it is a rendering.

---

## 2. When a refinement happens, and how often

**Only when something asks.** Phase 0 (`7bc4c60`) made a start refine nothing; the files it parses enter
`parsed_since_the_last_pass` (the backlog) and stay there. The askers are:

| asker | where | what it gets |
| --- | --- | --- |
| `catch_up` | every request's read path (`AnalysisState::prepare_for_a_read` also asks, and wakes the pump) | the request is answered from **L1**, the refinement is queued |
| a definition lookup that came from L1 | `Session::definition` | same |
| the diagnostics pull | `AnalysisState::prepare_for_a_diagnostic` | same, plus one `advance` whose tail may serve the ask inline |

**Once per file per session — measured, and made a test.** `Session::want_refinement` refuses when
`Session::is_refined`, and `is_refined` consults three things: the session's `refined_readings` set, the store's
refined marks, and the summary's own `env_hash`. Asking twice costs nothing:

```text
refined 1 of 1: 30.2 ms each
the same 1 ask(s) again: 0.002 ms each, 0 ask(s) queued
```

and the mark is **dropped with the reading**, so an edit re-arms it — `tests/refinement.rs` is the regression test:

```text
a_refined_reading_is_asked_for_again_after_an_edit ... ok
```

**Yes, once — but not once per environment.** The mark is per **path**, and the refinement is built from the file's
**own** closure (the file compiled standalone). A header included by two translation units with different macros is
refined once, from whichever closure was asked first, and the index has one reading for it. That is a deliberate trade
(`units_for_the_pass`'s note: a file's own timeline is its standalone closure) and it is the reason L2 is not keyed by
`env_set` in memory, while the **disk** entry is keyed by a closure fingerprint.

---

## 3. What a refinement costs, measured

```text
index of a 33-file project (302 files in the index, includes included)     2.0 s
  refine hello.c        169 words mentioned,   6 candidate files         30.2 ms      ← the design's order
  refine Uri.h          707 words mentioned,  27 candidate files         > 60 s        ← killed

index of a 103-file project (1 590 files in the index)                    5.0 s
  refine UriCommon.c    —                                                > 70 s        ← killed
                                                                         one run: "memory allocation of
                                                                         8589934592 bytes failed" (8 GiB)
```

```powershell
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle\3rd\uriparser"   # or the project itself
cargo test --release -p cpp_code_analysis --test refinement_probe -- --ignored --nocapture
```

**The cost is not a function of the file.** The same kind of file, refined by the same code, is 30 ms in a small index
and over two minutes in a large one. Two mechanisms in `prepare_the_re_read` explain the shape, and both are visible in
its own comments:

1. **The filter is "the words the re-read file mentions".** A C file mentions a few hundred words, a header a thousand;
   every file in the index that defines a macro of one of those names becomes a **candidate**, and each candidate's
   macro bodies are shaped. Common words (`NULL`, `size`, `MIN`, `MAX`) are defined in dozens of unrelated headers, so
   the candidate set is a property of the **index**, not of the file.
2. **An unplaceable body builds its defining file's closure environment** (`macros_from_the_closure_with_bodies`), and
   "nothing bodied" falls back to walking/rendering the **re-read file's own closure**. Both are closure walks, which
   the file's own comments measure at 4 127 ms per corpus and 17 747 ms for one drain of four files.

The 8 GiB allocation is not explained by either yet, and that is the first thing the next round should profile: it
happened while refining `3rd/uriparser/src/UriCommon.c` in the 1 590-file index, after the pass had decided something
was bodied, and the run aborted (`STATUS_STACK_BUFFER_OVERRUN`, `handle_alloc_error`) rather than finishing.

**The disk cache does not pay for this.** A stored refinement is a `FileSummary`; the reading the session now needs is
a **rendering**, so the store's own fast path refuses every entry it has:

```text
re-read: 16 stored refinement(s) are not served — a summary is not a rendering
```

Which means a **second start pays full refinement again**: the phase-2 change moved L2 from "a summary" to "a
rendering" and left the cache holding the old shape.

---

## 4. Who waits, and for what

**Nobody waits for a refinement itself.** Every caller that asks is answered from L1 and the ask is queued; the pump
serves it in a slice. What a request waits for is the **write lock**, and the pump holds it for the *insert* half only
— the parse/render half was moved under the read lock and measured (`docs/memory.md`, `handshake` latencies).

**The one place a request can pay a refinement inline** is `advance`'s tail (`finish_a_slice`, when `is_idle()`), which
`prepare_for_a_diagnostic` calls. On a 30 ms refinement that is invisible; on the > 70 s one it is a dead request, and
it happens under the write lock — the whole server waits.

**And the backlog is not work any more.** `pending_work()` counts the index queue, the cook backlog and the *wanted*
refinements — not `parsed_since_the_last_pass`. Measured after a start of the 103-file project:

```text
drained 25 slice(s) in 7.079 s: queue 0, cooking 0, re-read 1590      ← pending_work() == 0 with 1 590 owed
```

That is phase 0's contract stated as a number, and it is why `checks_may_be_trusted` cannot ask "is anything owed"
(see `docs/buglist.md` §28).

---

## 5. The design problems, in the order they cost

1. **L2's cost is `O(index)`, not `O(the file's closure)`.** The candidate filter is words, and words are shared across
   a project. On any project big enough to matter, the first refinement is seconds to minutes — so "refine when asked"
   is not a latency story until this becomes per-closure work. **This is the item to fix first**: a refinement's input
   should be the file and its include closure, and nothing else in the index.
2. **The cached L2 cannot be served** ("a summary is not a rendering"), so every start repeats the work. Either store
   the rendering (it is what L3 keeps anyway) or make the refinement a summary again; keeping a cache of the wrong
   shape is the worst of both.
3. **A request can pay a refinement inline, under the write lock** (`advance`'s tail from the pull path). Bounded
   work belongs to the pump; a request should ask and never serve.
4. **One reading per file, not per environment.** Cheap to say, hard to answer when two TUs disagree; the index cannot
   tell them apart today, and the hover card has no place to say "provisional".
5. **No "incomplete" marking.** Hover answers from L1 silently; a diagnostics pull publishes findings from L1 (the §28
   fix). LSP has `isIncomplete` on completion lists and used to have it on publish-diagnostics; the pull report has no
   such field, so the honest alternative is to *make L2 cheap enough that the question stops mattering* (item 1).

---

## 6. Where the room is, with the goal "low latency, low memory"

Ranked by expected win per unit of work, and each with the measurement that would settle it:

| # | idea | why it should win | what to measure |
| --- | --- | --- | --- |
| 1 | **Make a refinement closure-local**: candidates = the re-read file's include closure, not "files defining a word it mentions" | removes the `O(index)` term; the 30 ms case becomes the normal case | candidate count for a header in the 1 590-file index; refine time after |
| 2 | **Serve the stored L2**: key the cache by `(content, closure fingerprint)` and store the **rendering** (L3 is exactly that) | a second start refines nothing | `re-read: … not served` count → 0; second-start refine time |
| 3 | **Never serve a refinement from a request**: drop `advance`'s tail from the pull path, let the pump do it | a slow refinement stops being a stalled server | `worst_write_hold` before/after |
| 4 | **Refine the open files' closures eagerly, and nothing else** (the design's own order: open files, their closures, then the rest) | the first hover on an open file is the common case and would be L2 already | hover `wait gate`+`wait session` on a cold start |
| 5 | **Retire `parsed_since_the_last_pass` as the ask-list**: it is 1 590 paths on a warm session and no longer means "work" | one less unbounded-looking number in every report, and `re_read_backlog` stops lying | `MemoryReport` after start |
| 6 | **Cap `sources`**: the pass copies texts into a map keyed by normalized path; on the eager arm that is every indexed file | the pass's peak memory is the file texts it copied, not the index | peak RSS during a drain |
| 7 | **One `MacroEnvironment` per file, not per candidate body** (it is per candidate today, `get_or_insert_with`) | 27 candidates × closure walks → 27 walks, which is the > 60 s case | candidate walks per refine |

Nothing here is speculative about *whether* the work is there: items 1, 2 and 3 are measured defects with the numbers
above, and 4–7 are the follow-ons. The single measurement that would make the rest moot is item 1 — if a refinement is
30 ms for any file in any project, refinement can be eager again for the files a reader has open, and the whole
lazy-vs-eager argument in `docs/indexing-design.md` §3 can be re-opened with a number instead of a hunch.
