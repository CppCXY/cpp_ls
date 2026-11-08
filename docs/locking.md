# Why there are so many locks, and what the design should be instead

The reader's question is the right one: *"我们为什么这么多锁"*. This note counts them, says why each exists, and
proposes the shape that deletes most of them instead of trading them for staleness.

## What is there

```text
crates/cpp_ls    1 global lock     AnalysisState::inner   Arc<RwLock<Option<Session>>>
                1 gate             AnalysisState::gate    tokio RwLock, one writer at a time
crates/cpp_code_analysis  20        Session's fields       every one a Mutex
```

The twenty, by what they hold:

| Kind | Fields | Key |
|---|---|---|
| **content-keyed caches** — a value that is a pure function of `(text, environment, config)` | `renderings`, `views`, `macro_environments`, `rendering_summaries`, `walked_by_content`, `units`, `unit_roots`, `config_for_file`, `definitions`, `parsed_definitions`, `surfaces`, `effects` | a hash of the text (and of the macros in force) |
| **single-entry memos** | `checks_memo`, `unit_index`, `rendered_unit` | a generation number (`Option<(u64, T)>`) |
| **journals and queues** — "what has been done / what is left" | `refined_on_request`, `refined_readings`, `cooking_refused`, `walks_by_root`, `macro_work` | paths |

## Why each one exists

Not for parallelism: every one of the twenty is a `Mutex` **inside a session that is already behind the global write
lock** when it is mutated. They exist for one reason:

> **A query holds `&Session`, and it must remember what it computed for the next query.**

`Session::view`, `view_of_the_file`, `the_macros_of`, `checks_about`, `rendering_of` all take `&self` — that is what
lets a hundred queries run beside the pump — and every one of them fills a cache on the way out. `&self` cannot write a
field, so the field becomes a `Mutex`. That is the whole genealogy: **each lock is one patch for "the read path needs to
write"**, and there are twenty of them because that happened twenty times.

The global lock is the second half of the same story: because the read path *can* fill caches, and the pump *must*
change them, the only way to keep the two consistent was to put a reader/writer lock over everything. Then:

* a reader waits for the pump's in-flight slice (`commit_a_wave` holds the write lock);
* a request that needs to *fill* something (the file's summary after an edit, its cooked reading) takes the **write**
  lock, so it waits for the pump and the pump waits for it;
* and the fixes are of one shape: "check under the read lock, and only take the write if the work is really needed"
  (`prepare`, `prepare_for_a_read`, `catch_up`, `prepare_for_a_diagnostic`, the `caught_up` flag that just went away).

Every one of those checks is correct and every one of them is a symptom. A design where the question "do I need to
write?" has to be answered before a read is a design where the read path writes.

## The design that does not need them

The distinction the code is missing is between **state** and **products**.

* **State** is small and genuinely mutable: the text of each file, and the set of files.
* **Products** are everything else in that table: a summary, a rendering, a view, a macro environment, a cooked
  reading, a check finding. Each one is a **pure function of a key** — `(text, environment, config)` — which is why the
  caches are already keyed that way. A product is never *updated*; a new one is *published*.

So the session splits into a writer side and a reader side:

```text
AnalysisHost                                  the writers: the pump, didOpen/didChange, load
    texts:     Arc<Texts>                     path → (Arc<str>, Arc<LineIndex>)
    products:  Arc<Products>                  (path, text hash, environment id) → Arc<FileProducts>
    index:     Arc<ProjectIndex>              a fold over published products

Analysis                                      the reader: a clone of three Arcs, and nothing else
```

* **Publication is a pointer swap.** `didChange` builds a new `Texts` (one entry differs, files are `Arc<str>` so the
  copy is the map) and swaps it. The pump computes `FileProducts` for one file, swaps in a new `Products`. No reader
  is invalidated, no cache is cleared, nothing is waited for.
* **A query grabs the snapshot once** and works entirely inside it: no lock is held across the work, no lock can be held
  across it, because `Analysis` has no lock to hold.
* **Staleness becomes uniform and explicit**: a query answers from the snapshot it grabbed. If a newer one arrives, the
  server may re-run the query (that is rust-analyzer's "instant, then refined", and it is where the `refresh` a client
  already gets comes from) — but nobody *waits*.
* **The twenty `Mutex`es are gone**, not converted: a product cache is a field of `Products`, written by the one writer
  that builds the next version; the journals are the pump's own state; the single-entry memos are fields of the product
  they memoize.

## What it costs — stated, because it is not free

1. **Compute-then-publish instead of mutate-in-place.** Every mutation builds an immutable value. For the *products*
   that is what already happens (they are built from scratch per key). For the **index** it is the real work: today it
   is mutated in place under `&mut Session` (`insert`, `requeue`, scope postings), and a published index means either
   incremental per-file contributions folded on read, or a periodic rebuild. That is step 4 below and it is the one with
   a real price.
2. **Memory: one generation of products stays alive.** An `Arc` lives until the last in-flight query drops it. On this
   project that is the summaries and environments of the open files — a few MB, and the same few MB the caches already
   hold. The difference is that it is *bounded by queries in flight* instead of by an LRU's mood.
3. **Every query needs a staleness contract.** "May this answer be one publication old?" — written in one table rather
   than decided per handler, which is what the code does today and why the same symptom keeps reappearing under a new
   name ("`endl` has no hover", "诊断不改变", "I type `s` and it takes forever").
4. **"It is correct because it happened under the write lock" stops being an argument.** Correctness has to come from
   products being pure functions of their keys. That is the actual work, and the macro environment is the model: it was
   103.7 ms of closure walk per keystroke until it was keyed by the directives rather than by the content.

## Order of work, each step deleting locks

| Step | What it does | What it deletes | Price |
|---|---|---|---|
| 1 | `Arc<Texts>` + `Arc<Products>`, published by the pump; the four content-keyed caches (`renderings`, `views`, `macro_environments`, `rendering_summaries`) move into it | 4 `Mutex`es, and the write step in `prepare`/`prepare_for_a_read`/`catch_up` for the fetched-product case | a `Products` map with generation-retired entries; one `Arc` clone per query |
| 2 | the single-entry memos (`checks_memo`, `unit_index`, `rendered_unit`) become fields of the product they memoize | 3 `Mutex`es | the product is built once per key instead of once per generation |
| 3 | the journals (`refined_on_request`, `refined_readings`, `cooking_refused`, `walks_by_root`, `macro_work`) become the pump's own state | 5 `Mutex`es from the reader's path | the pump owns a struct readers never see |
| 4 | the index publishes per-file contributions; name lookups fold on read (or an `Arc<ProjectIndex>` is swapped per batch) | in-place `&mut` mutation, and the reason for the global lock's length | the fold cost on a cold answer; a rebuild policy |
| 5 | the server holds `ArcSwap<Analysis>` + a writer handle; `prepare_for_a_read`, `prepare_for_a_diagnostic`, `catch_up`'s guards and the `caught_up` flag are deleted | the global `RwLock` on the reader path | the handler code shrinks to "grab, query, answer" |

Steps 1–3 remove 12 of the 20 locks and every write a *request* takes, and none of them touches the index. Step 4 is
the one that needs care; step 5 is then mechanical.

## Step 1, as built — and the numbers

Landed in three commits:

* `ef8e9fb` — **a query may not publish the view it built**: `view_of_the_file` is a wrapper over
  `written_view(&self) -> Option<(FileView, ViewKey)>`, which publishes nothing, and `publish_the_written_view` is the
  writer's half, called from `advance` for the documents a reader has open.
* `e0ad2a1` (server side) — **the published texts**: `apply_buffer` publishes the buffer before it takes any lock, so
  `prepare_for_a_read` answers "is this file held" with no lock at all. That branch used to be a `try_read` on the
  session, and a lock it could not get sent the hover down `prepare`'s write path.
* `90a2adb` — **the four product caches became one `Arc<Products>`**: copy-on-write publication (`products()` clones the
  `Arc` and drops the lock; `publish()` copies the map it changes and swaps), readers on the new `Cache::peek`, and the
  pump as the writer. Four `Mutex`es become one `RwLock`.

Measured with `tests/codestyle_probe.rs::what_a_keystroke_costs_on_the_real_closure` on
`E:\EmmyLuaCodeStyle`, `CodeFormatServer/src/LSP/LSPHandle.cpp` (2181-token rendering):

```text
                       before step 1        after step 1
the first view          1.7–1.9 ms          48 µs          ← the pump published it
a body keystroke: view  1.7 ms              36 µs          ← 46×, and it is a hit
                  pump  15.6–18.8 ms        20.2–20.8 ms   ← the parse moved here
                  the project read          7.0 s          6.5 s
```

That is the trade the note argues for, made visible: **the view cost moved from the request to the background.** What is
left of step 1's price is that `catch_up` still writes when the summary is stale — the summary is an *index* product,
which is step 4's subject and deliberately out of scope here.


## The rule this leaves behind

> **A query may not write.** If an answer needs to remember something, that something is a product: give it a key,
> compute it in the background, publish it, and let the next query find it. A `Mutex` in a session that is already
> behind a lock is a product that has not been given a key yet.
