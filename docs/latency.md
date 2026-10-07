# Why the editor waits, and what the two real systems do instead

Written after a report that survived three rounds of fixes: **"依然非常非常慢"** — still very, very slow. The rounds
before it made the background cheaper and the numbers fell by a factor of forty, and the editor still waited. That
is the shape of a fix aimed at the wrong thing, and this document is the argument for what the right thing is.

Every number here was measured on this project, with the command that measured it named. Every claim about clangd
or IntelliJ is quoted from their own design documentation, linked at the point it is used. Where the two disagree
about *how*, they agree about *what*, and that agreement is the whole of the conclusion.

---

## 1. The symptom, and why the obvious reading of it is wrong

The user types `s` in a file that includes `<vector>`, `<string>`, `<format>` and `<iostream>`, and the completion
list takes seconds to appear. Measured over the wire with `target/scratch/completion_timing.mjs`, one open file:

```text
                                  before any of this work      after three rounds of it
  worst time a request waited            21 998 ms                      2 254 ms
  longest the writer was held            10 894 ms                        913 ms
  steady state, tenth request                  —                            9 ms
```

Forty times better, and still reported as "very slow" — correctly, because a keystroke's budget is around **16 ms**
and the tenth request is already there. The first nine are not, and a person types the first nine.

**The obvious reading** is that the background is still too slow: 913 ms is a long time to hold a lock. Three rounds
of work went into that reading, and each round was measured and each round helped. The mistake is not that the
measurements were wrong; it is that *this is not the quantity the user is waiting for*.

What the user waits for is the sum of two things, and only one of them was being attacked:

```text
  a request's wait  =  (the work the request does)
                    +  (the time until the lock the request needs is free)
```

Every round attacked the second term by making the holder let go sooner. **The first term is zero for the wrong
reason and the second term cannot go to zero while the request and the background take the same lock.** A
forty-fold improvement in a term that has a floor above the budget is a forty-fold improvement that is still over
budget. That is the arithmetic that three rounds did not do.

---

## 2. What clangd does

From **[Threads and request handling](https://clangd.llvm.org/design/threads)**, whose stated goals open with:

> respond to requests as quickly as possible (**don't block on unrelated work**)

That is the whole design in five words. The structure that implements it:

> `TUScheduler` … maintains a set of `ASTWorker`s, **each is responsible for one file**. … This ensures there's
> only one AST and one preamble per open file, **operations on one file don't block another**, and that reads see
> exactly the writes issued before them.

So there is no global object a request and the indexer both take. There are per-file workers, and a request about
one file is queued behind that file's own writes and nothing else.

And for completion specifically — the exact feature under report:

> Unlike typical requests like go-to-definition, code completion **does not use the pre-built AST**. … As this
> doesn't reuse the AST, **it can run on a separate thread** rather than the ASTWorker. It does use the preamble,
> but **we don't wait for it to be up-to-date**. Since completion is extremely time sensitive, **it just uses
> whichever is immediately available.**

Three separate decisions, all in the same direction:

```text
  it does not read the thing the background is building        (the AST)
  it does not queue behind the background                      (a separate thread)
  it does not wait for the background's current state          ("whichever is immediately available")
```

From **[The clangd index](https://clangd.llvm.org/design/indexing)**, how an answer can be good without waiting:

> `FileIndex` ("dynamic index") … This is the top layer, and includes symbols from the files that have been opened
> and the headers they include. … to ensure cross-references for the files you're working on are available, **even
> if the background index hasn't finished yet**.

and

> `MergedIndex` … layers one index on top of another. **Code implementing features sees only a single combined
> index.**

The background index is not made fast. It is made **irrelevant to the request**: the hot layer holds what the files
being worked on need, and the two are presented as one. A query reads the merge and never learns that the cold half
is incomplete.

## 3. What IntelliJ and CLion do

From the **[IntelliJ Platform SDK](https://raw.githubusercontent.com/JetBrains/intellij-sdk-docs/3dcf30b956ca96a32db073a22942310681f7e7bd/topics/basics/indexing_and_psi_stubs.md)**:

> Indexing is a potentially lengthy process. It's performed in the background, and during this time, **all IDE
> features are restricted to the ones that don't require indexes**: basic text editing, version control, etc.

with the mechanism named:

> This restriction is managed by `DumbService`. Violations are reported via `IndexNotReadyException` … It also
> provides ways of delaying code execution until indexes are ready.

The means are the opposite of clangd's — clangd answers from a partial view, IntelliJ **refuses to answer at all** —
and the property is identical: **a keystroke never waits for indexing**. IntelliJ pays for it in capability (a
feature is off until the index is up) and clangd pays for it in completeness (an answer may be missing a symbol the
background has not reached), and neither pays for it in latency.

Both are also worth reading for what they do *after* indexing, because that is where the hot path's speed comes
from: IntelliJ keeps a serialized **stub tree** — "a subset of its PSI tree, which contains only externally visible
declarations" — per file, so naming a symbol never means parsing the file that declares it. That is the same idea
as clangd's `FileIndex`, arrived at independently: **the thing a request reads is a small, per-file, already-built
summary, not the machinery that produced it.**

---

## 4. What this project does, stated in the same terms

```text
                            clangd                       IntelliJ                    this project
  what a request takes      per-file ASTWorker           the indexed stub/PSI        one global RwLock
  what a request reads      MergedIndex (hot + cold)     stub indexes                the same Session the
                                                                                     background is writing
  when the index is short   answer from the warm half    refuse, and say so          WAIT for it to finish
```

The third row is the defect, and it is visible in the code as `AnalysisState::settle`:

```rust
// crates/cpp_ls/src/context/analysis_state.rs
pub async fn settle(&self, cancel: Option<&CancellationToken>, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if self.with_snapshot(|session| session.is_idle()).unwrap_or(true) { return true; }
        ...
```

`session.is_idle()` is the **global** question — *has the whole project's indexing queue drained* — and eight
request handlers ask it with a **two-second** budget (`crates/cpp_ls/src/handlers/*/mod.rs`). clangd's answer to the
same question is written in as many words: *"we don't wait for it to be up-to-date."*

The second half of the defect is the lock. `run_blocking` takes a read lock on the same `RwLock` the pump takes for
writing, and on Windows the underlying `SRWLOCK` is **writer-preferring**: a reader that arrives while a writer is
waiting queues behind it. So a request waits for the *whole* of whatever the writer is doing, and the writer is
doing, measured, 95–913 ms of second-pass re-reads and index inserts. Shortening that is the three rounds above.

### 4.1 Why `settle` was added, and why that reason does not survive

The comment on the completion handler states it honestly:

> The list is built to degrade rather than lie — a name whose declaration has not been read is skipped — so an index
> that is still filling gives fewer items and never wrong ones. … opening a file showed a popup of **nothing but
> keywords** while hover worked, which is precisely what "fewer" looks like when there is nothing to be less than.

So the answer without waiting was **bad**, and waiting was the fix. clangd's design says the bad answer is the thing
to fix: its `FileIndex` exists precisely so that "the files you're working on" are *always* in the index, and the
popup is never empty whatever the background is doing. **We removed the symptom by waiting; the cause is that the
hot set is not kept hot.**

---

## 5. The design

Three changes, in order of how much of the wait each removes. Each has an acceptance number, and the numbers are
what "done" means.

### 5.1 A request never takes the lock the background takes

**Today.** `AnalysisState::inner: Arc<RwLock<Option<Session<DiskFiles>>>>`, taken for reading by every query
(`run_blocking`) and for writing by every pump step (`update`), with the pump's write guard held across the whole
step — measured at 95–913 ms after the three rounds.

**Change.** Publish what a query needs as an **immutable snapshot**, shared by `Arc`, and let a query load it with
one atomic:

```rust
pub struct AnalysisState {
    /// **What a request reads.** Replaced whole by the pump, loaded with one atomic by a query — no lock, no
    /// queue, nothing to wait behind. This is the change that makes the wait zero by construction rather than small
    /// by tuning.
    published: ArcSwap<Published>,
    /// The pump's own session. **No query ever takes this lock** — that is the invariant the whole design rests on.
    working: tokio::sync::Mutex<Option<Session<DiskFiles>>>,
    ...
}

pub struct Published {
    /// The index. Already the largest thing a query reads, and already behind `Arc` once this change is made.
    index: Arc<ProjectIndex>,
    /// The buffers and the disk files. **Already a shared handle** — `SessionFiles` is documented as "a map behind a
    /// lock that clones share" — so publishing costs a clone of the handle and not of the files.
    files: SessionFiles<DiskFiles>,
    /// Everything else a query reads and the pump writes: the configuration, the declaration list, the pending
    /// flags.
    view: Arc<SessionView>,
}
```

**What makes this cheap.** A unit is already `Arc<TranslationUnit>`. The VFS is already a shared handle. The one
big thing that is *not* shared is `SummaryStore::index: ProjectIndex`, held by value — and it is the only reason
publishing would cost anything. Putting it behind an `Arc` is the enabling step: the pump is the **only** writer,
so `Arc::get_mut` succeeds and it mutates in place, while a query holding an older `Arc` keeps reading a consistent
index. (This is the point where a purist would want a persistent map so that old and new are genuinely different
allocations. For this codebase the pump is a single task and queries only ever read, so a shared `Arc` plus the
pump's own exclusive access is enough — and it is worth writing down that this is a deliberate weakening rather than
an oversight.)

**Acceptance.** A completion asked while the index is filling comes back in **under 50 ms**, at every attempt from
the first — measured by `crates/cpp_ls/tests/latency.rs`, which already fires twelve of them at `didOpen` and today
fails with 703–2254 ms.

**What it costs.** The invariant "a query never takes `working`" has to hold everywhere, and it is exactly the
invariant that `settle`, `prepare` and `catch_up` break today. Each of them is re-expressed against `published`.

### 5.2 The wait becomes a question about one file

**Today.** `settle(is_idle)` — the global question — with a two-second budget, in eight handlers.

**Change.** What a request needs before it can answer is **its own file**, not the project: the file's text is in
the VFS (`prepare`, one file) and its summary is in the index (`catch_up`, one parse). Both are already
per-request work that happens before the query. After them the global question is not a prerequisite for anything.

So `settle` loses its callers rather than getting a smaller budget. clangd's rule applies literally: *use whichever
is immediately available*.

**Acceptance.** `settle` has no callers on the request path, and the latency test still passes — which it only can
if the per-file work really is enough.

### 5.3 The hot set stays hot

**Today.** The pump indexes the project's closure in queue order, and a request for a header it has not reached
gets whatever is there.

**Change.** The file a request names, and the includes that file names, are **moved to the front** — which
`want_the_closure_cooked` already does for cooking and nothing does for indexing. This is `FileIndex`: the hot
layer is the open files and their direct includes, and it is what makes 5.2's "immediately available" a good
answer rather than an empty one.

**Acceptance.** Opening a file that includes `<string>` and immediately asking for completion after `std::` returns
`string` and `basic_string` — the measurement in `want_the_closure_cooked`'s comment, which records that a raw
reading of `<xstring>` gives 23 items and no `std::string`, and that one level further gives `basic_string`.

---

## 6. Order, and what each stage is judged by

Each stage ships on its own and is measured on its own. The order is by the size of the wait removed, and no stage
depends on a later one.

```text
  1.  the index behind `Arc`, the snapshot published, a query loading it atomically
      judged by: the write lock's hold time stops appearing in any request's latency
                 (the detector at `AnalysisState::update` already reports it; today it is 95–913 ms)

  2.  `settle` off the request path
      judged by: `latency.rs` green, and no handler calls `settle`

  3.  the hot set moved to the front of the index queue
      judged by: completion after `std::` in a file that includes `<string>` offers `std::string`

  4.  the second pass re-reads what it must, and nothing else
      judged by: the budget warning stops firing at all, so stage 1's snapshot is not merely hiding the cost
```

**Stage 4 is deliberately last.** It is where the three rounds of work already went, and it is the stage that is
*not* needed for the user's complaint. It matters because a snapshot hides a slow writer from a reader and does not
make the index complete any sooner — and an index that is complete later means stage 3's answer arrives later. So it
is real work, kept in the plan, and placed after the stages that make the editor usable.

---

## 7. What this document is not

It is not a plan to make indexing fast. clangd's own background index is documented as taking **multiple hours** on
Chromium-sized projects, and it does not matter, because no request waits for it. The target here is the same one:
**an index that takes as long as it takes, and an editor that is quick while it runs.**

It is also not a claim that the three rounds were wasted. They took the worst wait from 21 998 ms to 2 254 ms and
the worst lock hold from 10 894 ms to 913 ms, and they put a detector in the product (`AnalysisState::update`
reports every hold over 50 ms, with the name of the call site) and an end-to-end test in the repository
(`crates/cpp_ls/tests/latency.rs`). Stages 1–3 are only checkable *because* those exist: without the detector there
is no way to tell a wait in the queue from work in the request, and that distinction is the whole of section 1.
