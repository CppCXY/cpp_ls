# The design, read against clang, CLion and Visual Studio

An outside review. `docs/design-review.md` judges this codebase against its own plan; this one judges it
against the three tools it says it is learning from, and against what it currently does. Every claim below
is either a measurement taken on this machine on 2026-10-09 (the command is given) or a `file:line` in this
repository. Where a claim comes from another tool, the upstream source is named, and it is named because the
*mechanism* is the lesson — not the tool.

The short version, and it is not the version the documents tell:

```text
  the ENGINE is ahead of the field in three specific places (the compiler oracle, the refusal
  vocabulary, the recovery invariants) and behind it in one (there is no template instantiation).

  the SERVER is behind clangd on every axis that is about a person using it: it takes 6–27 s to
  answer a completion while the index fills, it has no incremental sync, no code actions, no
  per-feature readiness, and the reason a query failed never reaches the client.

  the DOCS are the best artifact in the repository, and they have drifted: three test targets fail
  right now, and the two that matter are the numbers this project exists to improve — they are not in
  `status.md` at all.
```

---

## 1. What we are doing right

### 1.1 We reached clangd's preamble key independently, and ours is stricter

`RootKey::Preamble { end, hash }` (`crates/cpp_code_analysis/src/tu_cache.rs:75-115`) is clangd's
`PreambleBounds` + `PrecompiledPreamble::CanReuse` arrived at from the other direction. clangd compares the
preamble region **byte for byte** and validates every entered file by **(size, mtime)**; we hash the region
and hash every entered file. Both refuse on the same condition — "an edit below the bound cannot change what
the walk reads" — and ours cannot be fooled by an mtime-preserving copy.

The refinement we have that clangd does not is the **tail rule** (`tu_cache.rs:107-111`): a `#` anywhere
below the bound refuses the entry even though it is provably not a directive. clangd handles the same case
by re-lexing; we refuse, which is the direction this codebase always chooses. Keep this. It is the single
best-decided thing in the cache layer.

### 1.2 The compiler is the oracle, and that is not something either reference tool does

`crates/cpp_code_analysis/src/align.rs` runs a real `-E` over the same file, puts the two token streams side
by side, and classifies every difference **by the mechanism that produced it** (`Difference::reason`,
`align.rs:41-46`) rather than by which tokens are on either side. Two properties make it worth more than the
code around it:

* it is the only measurement in the repository that cannot be argued with — the compiler either chose the
  same branch or it did not;
* the two entry points are pure (`align.rs:59-64`), so the *comparison* is tested against recorded `-E`
  output on a machine with no compiler, and only `examples/align_preprocessor.rs` touches the world.

clangd has no equivalent: it *is* the compiler, so it has nothing to align against. CLion's code model and
VS's EDG-based engine have internal differential harnesses, but neither is reproducible from outside. **This
is our one genuinely novel instrument, and §6.1 is about the fact that nothing runs it.**

### 1.3 Failure is a value, the vocabulary is closed, and it is not watered down

Twelve `UnknownReason` variants (`crates/cpp_code_analysis/src/sema/symbol.rs:136-215`), a closed `Known<T>`
(`:60-64`) with the closure argued at `:129-134` ("a new source of doubt has to be added here, and every
`match` then fails to compile"), and a check layer whose contract is explicit
(`crates/cpp_code_analysis/src/sema/check/mod.rs:15-23`): `Yes`/`No` report, `Unknown` reports **nothing**.

Three pieces of evidence that this is real rather than aspirational:

* **No "pick the unique name" fallback exists anywhere.** I looked for it specifically, because it is the
  obvious cheat and every immature index has one. `ProjectIndex::definition` returns `Ambiguous` for a
  multi-declaration answer (`index/project.rs:6259-6266`), and `semantic.rs:490-491` records what that cost
  before the member path existed: `v.push_back` and `t.size()` got **no colour at all** — the layer refused
  to guess.
* **A check was written, measured and deleted.** `no_object_has_type_void` produced 10 false positives on
  `omp_llvm.h`, the root cause turned out to be a *fact* defect rather than a check defect, and the check was
  removed with the reasoning kept (`check/mod.rs:80-96`).
* **The measurement is taken per check, not in total** (`check/mod.rs:156-169`), including the record of the
  day a total was misread as one check's number. This is the discipline clangd's `TidyFastChecks.inc` shows
  from the other end: a cost is a property of one check, measured on a fixed corpus.

### 1.4 Recovery is structurally total, and it is fuzzed as such

`crates/cpp_parser/tests/invariants.rs` asserts four invariants — losslessness, well-formedness, idempotence,
totality (`:1-16`, `:49-152`) — plus `recovery_leaves_the_event_stream_balanced` (`:801-847`), and the two
that matter are the cheap ones to run and the expensive ones to satisfy:

```text
  every_prefix_of_every_sample_is_parseable   every char boundary of the whole corpus, I1 and I2 per prefix
  single_character_deletions_are_parseable    the mutation a person actually makes
```

clang's own recovery is better (typo correction, `RecoveryExpr`, diagnostic suppression after an error) but
it is not *checked* this way; clangd inherits clang's and tests it far less. When VS's IntelliSense meets an
unparsable file it shows what it can and stops explaining; ours has a proof obligation that no input can
produce a lost token. **This is the best-engineered part of the project and the part most worth protecting.**

### 1.5 The rest of the ledger

| mechanism | who does it | where we stand |
|---|---|---|
| per-file, content-addressed index artifact | IntelliJ stub trees, clangd shards, both independently | **have it** — `SummaryStore`, `SummaryKey` (`cache.rs:285-333`), and the key is computable **before** parsing (`index/store.rs:19-39`) |
| artifact validity without re-stating the world | clangd `PreambleFileStatusCache` | **partial** — we have the key, not the cheap path; the closure is re-read and re-hashed on every validation (`tu_cache.rs:164-195`, priced at ~60 ms for the SDK corpus in `tu_cache.rs:27-29`) |
| reader-fingerprint so a parser change cannot serve a stale reading | clangd rejects on a format version (`index/Serialization.h`) | **ahead** — `READING_FINGERPRINT` (`cache.rs:110-112`) is computed by `build.rs` from the *source of the readers*, so a change to a reader invalidates by itself |
| never cache when the filesystem decided the answer | clangd `HadErrors` shards retried later | **have it** — an unresolved include is built and **not written** (`index/store.rs:41-56`) |
| open files before project files | clangd `boostRelated` / priority queues | **have it** — `Worklist`'s two queues, open half first (`index/worklist.rs:1-42`) |
| a refusal to answer while indexing rather than a wrong answer | IntelliJ `DumbService` / `IndexNotReadyException` | **have the refusal, not the granularity** — and in `references`/`rename`/`workspace_symbol` only (§3.3) |
| one writer, in the client's order | — | **have it, better than clangd** — a request applies the notification queue up to its own dispatch sequence (`context/update_queue.rs:20-46`) |
| coordinate mapping written↔compiled | clangd spelling vs expansion locations | **have it, and it is centralised** — `util/position.rs:41-70`, tested through a real parse including a surrogate pair |
| `isIncomplete` on a partial answer | clangd completion | **one handler only** — `completion/mod.rs:191`; `semantic_token` and `signature_help` answer `null` while pending instead |

---

## 2. The four hardest findings

### 2.1 A completion during indexing takes 6–27 seconds, and the cause is documented and unfixed

```text
cargo test -p cpp_ls --test latency comes_back_within_the_budget -- --nocapture

  #0 6297 ms   #1 2496 ms   #2 10833 ms   #3 26697 ms   #4 14038 ms
  #5   58 ms   #6   49 ms   #7    57 ms   … every one 131 items
```

The budget is 2500 ms (`tests/latency.rs:63`), the steady state is asserted at 50 ms (`:73`), and the
steady state is **met** — 49–59 ms from the sixth request on. What is not met, by an order of magnitude, is
the window the test exists for: the first five completions after `didOpen`, which is exactly the window a
person types in.

The cause is not a mystery and it is not in the test. It is written in the code that causes it
(`index/project.rs:540-581`):

> **This is the largest remaining term in a completion's latency, and it is the reason a keystroke is slow
> exactly while the index is filling.** Every `insert` drops the whole memo, so the next query re-evaluates
> every guarded include in its closure from scratch — and during indexing there is an insert every few
> milliseconds, so *every* query pays it.

`visibility_answers` is cleared whole at `index/project.rs:5178` and `:5351`; the code even names the fix
(remove by file, via `dependents_of(inserted)`) and says it is not written. The stale numbers beside it say
**350/598/1270 ms**; the measurement above is 6–27 s.

This is the mechanism that clangd's `FileIndex` split exists to avoid: preamble symbols and main-file
symbols are two independently-swapped indexes with monotonic version counters, so a frequently-changing
layer never invalidates an expensive one (`index/FileIndex.h`). We have one index and one invalidation rule,
and the rule is "everything". **Everything else in this document is smaller than this.**

### 2.2 Three test targets fail at HEAD, and two of them are the numbers this project is for

```text
cargo test --workspace --no-fail-fast

  cpp_code_analysis --test expand      40 passed / 2 FAILED    panicked at preprocess/expand.rs:786
  cpp_ls --lib                          50 passed / 2 FAILED    references.rs:300, rename/mod.rs:297
  cpp_ls --test latency                  0 passed / 2 FAILED    the two tests §2.1 is about
  every other target                    green — 1 100+ tests, including cpp_parser/tests/invariants.rs
```

`docs/status.md:232-240` reports `594 passed / 0 failed` for the lib, "all 25 analysis suites green",
`50 passed / 2 failed` for `cpp_ls` lib, and one flaky *end-to-end* test. That was true when written. Today
the `cpp_ls` lib failures are still there (so `status.md` is accurate about them) and two new ones are not
mentioned: `expand` and `latency`, neither of which the document lists at all.

**The `expand` failure is a real defect, not a test artefact.** Two tests panic with `attempt to subtract
with overflow` at `crates/cpp_code_analysis/src/preprocess/expand.rs:786`:

```rust
last.token.range.end_offset() - first.token.range.start_offset     // the region of a token run
```

`span_of` (`expand.rs:869-877`) computes the same thing and carries a comment stating the assumption that
breaks:

> A run is contiguous in a file only when it came from one: a substituted body mixes tokens from a
> `#define` with tokens from a call site, and the span of such a run covers both.

The failing fixtures are `#define F(x) [x]` / `#define A F` / `B (7) ;` — the `Tail` mechanism
(`expand.rs:824-852`) reaches past the invocation for the arguments of a function-like macro that a macro
body ends in. Tokens taken from the tail can sit *before* the body tokens in the file, so the subtraction
underflows. In a debug build this is a panic in the preprocessor; **the shipped server is a release build,
there is no `[profile.release]` in `Cargo.toml`, and no `overflow-checks` setting anywhere in the
workspace**, so in release the same expression wraps to a near-`usize::MAX` length and a `SourceRange` that
covers the file. The test is telling us that a region is being computed from an assumption the code's own
comment says is false.

**The `latency` failures are the project's headline claim, failing.** `latency.rs:34-77` is written as the
place where "we should keep it fast" becomes something a change can fail, and §2.1 is what it caught.

### 2.3 One question, two answers — the defect shape the project has already paid for three times

`docs/status.md:119-126` names the pattern and it has recurred:

> Two implementations of one question, and only the first was told about the rule. That is the shape this
> defect has taken three times now.

Here are three more, all at HEAD:

**(a) "The region of a token run" — two implementations, one with a comment saying the assumption is
false.** `expand.rs:783-789` and `expand.rs:869-877` are the same computation written twice; the second has
the caveat, the first does not, and the first is the one that panics (§2.2).

**(b) "Does this class have this member?" — two rules in one function.**
`index/project.rs:331-333` states the rule:

> a type that **is** known and does not have the member is not that case: answering with another class's
> member of the same name would be a jump to a place the language does not name, which the handler's own
> rule ("a wrong location is worse than none") forbids.

`index/project.rs:345` then does exactly that — `Known::Unknown(_) | Known::No => {}` falls through to the
name query. The block at `:337-344` is a *third* statement, that this is "a deliberate trade rather than an
oversight". All three are in one function, and the code implements the one the first block forbids. The
mechanism that makes the fall-through unavoidable is elsewhere: `member_definitions_across_files` returns
`Unknown(NotDeclaredHere)` both for "the class lacks the member" and for "a base could not be read"
(`:279-299`), so the caller *cannot* tell the two apart. **The fix is not a rule, it is a distinct answer.**

**(c) "What is a partial specialization's member type?" — first match wins, and it has been measured
producing a wrong type.** `index/project.rs:4675-4693`, in the project's own words:

> Measured: `std::shared_ptr<int> sp; std::atomic<int> counter; auto n = counter.load();` answered
> **`shared_ptr<int>`**.

Two fixes were tried and rejected (`:4699-4704`) and there is still no partial ordering — the choice is a
`.find(...)` (`:4705-4709`). This one is mitigated by `a_pattern_matches` and is not the same shape as the
other two: it is a *known* wrong answer with a *stated* mechanism. It is on this list because the mechanism
is the one clang solves with overload/partial-ordering machinery, and because a wrong type "offers the wrong
members and silently mis-navigates every access built on it".

### 2.4 The three-valued answer never reaches the client

`README.md:128-131` claims:

> Every answer is three-valued — yes, no, and unknown-with-a-reason — and the reasons are specific… A
> consumer can tell a missing answer from a wrong one, which is the difference a diagnostic depends on.

On the wire, `Known::No` and `Known::Unknown(reason)` both become `RequestOutcome::Missing`
(`cpp_ls/src/context/mod.rs:106-108`) and both become JSON `null` — `definition/mod.rs:122-124`,
`hover/mod.rs:171`, `references/mod.rs:121`, `rename/mod.rs:139-141`. The reason goes to `log::debug`. The
one place a reason is transmitted is a module-import `INFORMATION` diagnostic
(`handlers/diagnostic/mod.rs:102-137`).

This is the cheapest high-value change in the document. clangd shows unresolved-name reasons through
IncludeFixer and the index; VS shows "cannot open source file" as a squiggle; IntelliJ shows unresolved
references as *inspections*, which is to say the same three-valued fact rendered as a UI element. We have
the vocabulary and we throw it away one layer above where it is computed. A `null` completion is
indistinguishable from "the index has not read that header", which is the exact failure mode the
`settle`-removal round spent a week learning to remove from the popup
(`cpp_ls/src/context/analysis_state.rs:43-65`, `docs/status.md:143-156`) — and `null` is still the answer
for every other handler.

---

## 3. The learning ledger: what each tool does that we do not

### 3.1 clang and clangd

**No template instantiation, and it is the dominant refusal class** (`docs/status.md:54-55` says so).
clang's answer is `Sema::DeduceTemplateArguments` → `InstantiateFunctionDefinition`, with the substitution
table built by `TemplateDeductionInfo` and two-phase lookup splitting dependent names until instantiation.
We have `Type::substituted` (`sema/types.rs:384`), which pairs by name positionally, four call sites driving
it, and a stated limit (`:373-383`): *"This is not instantiation — no body is re-read, no overload is
chosen, no dependent name is resolved."*

The lesson is not "implement instantiation" — that is a compiler, and §7 of `design-review.md` is right that
it is out of reach. The lesson is **which half is worth having**: clang separates *deduction* (work out `T`
from the arguments) from *instantiation* (re-read the body with `T` bound). Our status document already
frames the next item as "re-evaluate an `auto` inside a function template's body against the enclosing
template's substitution table" — that is exactly the deduction half, applied to one question, and it does
not need a body re-read. **Keep the scope there and resist the pull toward a general `instantiate`.**

**Overload resolution, ADL and access control are absent entirely** (`sema/resolve.rs:12` states it). The
grammar parses overload sets into `Vec<Binding>` (`sema/symbol.rs:707`) and never selects. clang's answer is
a ranked candidate set with conversion sequences; the affordable subset for us is the *existence* question —
"this call has candidate functions but none is viable" — which is what a squiggle needs and what
`an_argument_does_not_convert` approximates today.

**`SkipFunctionBodies` is a trap we have already sprung.** `docs/design-review.md` §3.3 records that we
built body-skipping, measured it at zero wall-clock gain, and lost three capabilities. clangd's own FAQ
concedes the false positives it causes. We are *ahead* here: we measured before copying, and the answer was
no. The mechanism to take instead is clangd's **two artifacts for two consumers**
(`design-review.md` §3.4): an index that never sees a body and a full AST for the open file. We have the two
consumers and one artifact. That is still the right architectural item, and it is now the *only* one left.

**Function-body skipping is not the only per-file filter clangd uses.** `SymbolCollector::FileFilter` +
`digestFile` (`index/Background.cpp`) means re-indexing one TU re-collects only the files whose digest moved.
We re-collect per file already (§1.5) — this one we have.

**PreamblePatch** (serve a stale preamble by replaying the directive delta) is a workaround for clang's PCH
model, and the research is unambiguous that it should **not** be copied: clangd's own code says directive
scanning "deliberately ignore[s]" conditional directives and `#undef` in the preamble region, and the
`#line` mechanism has Windows backslash-escaping problems. Our macro timeline plus written-at/report-at
token origins carries strictly more information than `#line` can. Copy the *goal* (an edit inside the
include block should not cost a closure walk — which `RootKey` already gives us) and not the mechanism.

**ASTWorker's liveness predicate** is the one clangd mechanism we have no analogue for at all
(`TUScheduler.cpp: shouldSkipHeadLocked`): a queued re-analysis is dropped if a later one is already queued
**and** its diagnostics are not required, cancelled reads jump the queue so the caller gets its error
promptly, and cancelled updates are downgraded rather than discarded. We have one global lock, one pump and
one queue; §3.2 is why that matters.

**Per-file serialization** (one thread + one queue per open file) is the structural answer to "editing two
files should not make one pay for the other". We have one `RwLock<Session>`
(`cpp_ls/src/context/analysis_state.rs:78`), so a commit for file A and a commit for file B serialize on the
same lock, and `didClose` clears the **whole** 16-entry unit table (`cpp_code_analysis/src/session.rs:1102`).

### 3.2 The server layer: the gap that is not about C++ at all

This is where we are furthest behind, and none of it needs a better parser.

```text
  incremental sync          we advertise FULL (cpp_ls/src/handlers/text_document/mod.rs:28)
                            clangd, CLion and VS all apply ranges; our own update_queue
                            comments say full sync is what makes coalescing safe
  work-done progress        exists for LoadWorkspace/DiagnoseWorkspace only, reports "N files to read",
                            never "N of M" (handlers/initialized/mod.rs:417-438)
  per-feature readiness     ONE signal. clangd reports index status; IntelliJ's DumbService makes
                            it a per-feature property (DumbAware); VS splits live engine from
                            database-only features. design-review.md §3.6 already names this
  cancellation              cancels the answer, never the work: the token is polled before and
                            after the blocking query (context/query_runner.rs:49-75); a running
                            Session::advance cannot be interrupted at all
  memory accounting         no byte accounting anywhere; clangd keeps 3 ASTs (ASTRetentionPolicy)
                            and reports UsedBytesAST/UsedBytesPreamble per file
  code actions              none. No codeAction, no resolve, no applyEdit path
                            (context/client.rs:165-181 is dead code)
  navigation breadth        no declaration / typeDefinition / implementation / documentHighlight /
                            callHierarchy / typeHierarchy (~23 methods answer -32601)
```

Two of these are worse than they look:

**`references` answers the wrong question and says so in the code.** `symbol_references`
(`index/references.rs:441-494`) resolves the cursor to a symbol (`SymbolToFind`) and then collects *every
identifier token in the candidates whose spelling matches* (`:478-492`), labelling each
`ReferenceKind::Use { resolved_to: symbol.declared_in }`. That label is a claim, not a resolution. Ask on a
common name and every same-spelled identifier in every transitively-including file is presented as a use.
The doc block above it (`:413-421`) describes the *failure mode this reproduces* ("a user who renames on
that answer changes the declaration and nothing else") as the thing that was fixed. clangd answers this from
a resolved AST (`Decl::getCanonicalDecl` identity); IntelliJ answers it from `PsiReference.resolve()` plus
the stub index; VS from the browse database plus a resolve pass. Ours is a lexer scan with a resolved
*subject*. Renaming is correctly gated to macros (`handlers/rename/mod.rs:119-152`), so the *edit* is safe —
the *list* is not, and `docs/status.md:181-198` presents it as a finished feature.

**`hover` has a stated defect with no logging behind it.** `handlers/hover/mod.rs:119-120` names the case
(`void f() {}` declared in the file itself has no hover) and promises "the logging below"; there is no
`log::` call in the file. `docs/status.md:136` lists hover as "works".

**Two caches grow with every keystroke and nothing evicts them.** `Session::macro_environments`
(`session.rs:499-504`) and `Session::renderings` (`:518-523`) are `HashMap`s keyed by
`(path, content_hash)` — so a new entry per edit — and neither has an eviction path: the only removes are
`didClose`. `MAX_UNITS = 16` bounds the *unit table* (`session.rs:4504`, eviction at `:4535-4544`), which is
the right shape applied to one of three caches. A macro environment holds a closure's worth of bindings; a
rendering holds a whole cooked token stream. clangd bounds ASTs at 3 and reports bytes per open file
(`TUScheduler::FileStats`); IntelliJ bounds stubs by the file's own size and drops the PSI cache on
eviction; VS auto-tunes its translation-unit cache to available RAM (2–64). We have the strongest story of
the three on *disk* (a 2 GiB prune, `cache.rs:150`) and no story at all in RAM.

**Three documentation pointers are dangling:** `docs/latency.md` is cited ten times across the handlers and
by `tests/latency.rs:67,208` as the authority for the keystroke budget, and it does not exist; `.gitignore`
cites `docs/index-design.md`, which does not exist; `docs/incremental-edits.md:427-428` cites
`session.rs:4083, 4111` for the unit table, whose real lines are `4504/4519`.

### 3.3 CLion / the IntelliJ platform

**`DumbAware` is the single most valuable idea we have not implemented, and it is cheap.** IntelliJ's rule
is that a feature declares whether it needs a complete index; `CompletionContributor` and `Annotator` are
dumb-aware, so completion and highlighting keep working while index-dependent navigation does not. Our
equivalent is one flag plus `isIncomplete` in one handler. `references`, `rename` and `workspace_symbol`
already *refuse* while work is pending (`references/mod.rs:281-318`, `rename/mod.rs:270-306`,
`workspace_symbol/mod.rs:146-169`) — that is the right instinct and it is the wrong granularity: it refuses
one feature at a time with no statement to the client about *why*, and no feature that could degrade
continues.

**Stub trees' invariant is the rule our summary layer is missing.** IntelliJ's contract is that *"all
information stored in the stub tree depends only on the contents of the file for which stubs are being
built"*. `docs/design-review.md` §3.2 states the gap: our `SummaryKey` includes `context_hash`, and the
timeline is keyed on the closure. The rule to take is not "make summaries closure-independent" — the
timeline deliberately is not — it is **the invalidation unit should be a file**: an edit in one header
should not require re-reading the closure of every file that includes it. We do this for the *root*
(`RootKey`) and not for anything else.

**Index infrastructure: incremental reparse of the edited file is deliberately absent in the platform too.**
IntelliJ reparses only the changed ranges when it can and falls back to a whole-file reparse; the *stub* is
rebuilt whole, always. That is evidence for §1.5's "do not promise incremental re-analysis" — the cheap win
is whole-file re-parse of the edited file plus amortized reuse of everything it includes, which is where our
`RootKey` already is.

### 3.4 Visual Studio

**The browse database is a separate artifact from the parse, and it is allowed to be stale.** VS keeps
`Browse.VC.db` for navigation and the live IntelliSense engine for the open file, and it lets the database
lag — the default is *not* to wait for it. That is the same split clangd makes with
`FileIndex`/`BackgroundIndex`, and the third independent statement of §3.1's closing item. Three tools, one
architecture, and we have one artifact where they have two.

**"Wait for the browsing database" defaults to off.** The user-visible consequence of that default is a
product that answers immediately and is sometimes incomplete, rather than one that is complete and late.
Our `settle`-removal round reached the same conclusion from a bug report and clangd's own words
(`analysis_state.rs:60-65`); VS is the strongest evidence that the *default* matters more than the
mechanism.

**VS re-stats the solution on a timer (60 minutes) rather than watching every file.** We use the client's
watcher (`handlers/text_document/watched_file_handler.rs:1-16`), which is better informed. This one we have.

---

## 4. Documentation drift

The documents are the repository's best asset and the review's most uncomfortable section, because the drift
is not sloppiness — it is the *cost of the writing style*. Every doc here is written as a snapshot with
numbers, and a snapshot with numbers decays the moment the next commit lands. Four specific failure modes,
each with instances at HEAD:

**(a) A measurement is stated as a contract, and the code moved on.** `analysis_state.rs:96-97` says a
request's wait is bounded "**by construction**, because there is no other lock on the path". The enforcement
is a `log::warn!` (`:422-428`) and a recorded maximum (`:421`) — nothing splits or defers a long update. The
same file records slices that held the analysis for **84–2430 ms** and a zero-file drain for **1527 ms**
(`handlers/initialized/mod.rs:77-78`), against a `WRITE_BUDGET` of 50 ms. `design-review.md:143` then
describes `WRITE_BUDGET`/`QUERY_BUDGET` as what stands in for per-capability readiness. They are
*instruments*, and good ones; they are not enforcement, and the difference is what §2.1's 27 seconds is made
of.

**(b) The fix is named in a comment and the doc does not know.** `index/project.rs:565-580` names the
`visibility_answers` fix and says it is not written; `tests/latency.rs:57-62` names it as "the next round's
work"; `docs/status.md` and `docs/design-review.md` — the two documents written to be read *before* the next
round — do not mention it. The single largest latency term in the product lives only in the source.

**(c) Two documents describe the same mechanism as implemented and as not started.**
`analysis_state.rs:60-65` and `incremental-edits.md:863` disagree about the hot set, and `session.rs:130-133`
and `session.rs:466-474` disagree about whether the unit walk happens under a read lock or a write one. The
third instance is subtler and is the one worth acting on: `analysis_state.rs:148-150` tells the reader a read
"does not wait for the pump's parse", and `:291-295` in the *same file* explains that on Windows a waiting
writer queues new readers behind it. Both are true of different scenarios and neither says which — so a
reader who wants to know why a completion waited has to hold both in mind and work out which one applied.
The number that settles it exists (`worst_write_hold`); the prose does not point at it.

**(d) A cheap number is quoted once and never taken again.** Two, both measured today:

```text
  claim                                              doc                    today
  `cargo test -p cpp_code_analysis --lib` finishes    status.md:354          4.76 s (594 passed)
     in 0.43 s
  workspace zero warnings                             status.md:240          16 clippy warnings over the lib
                                                                             and bin targets (13 in
                                                                             cpp_code_analysis, 3 in cpp_parser)
```

The warning row is worth a sentence because of what it is not: `cargo build` really is silent, which is what
the claim was about. But there is no `clippy.toml`, no `[lints]` table in any `Cargo.toml` and no
`#![warn]`/`#![deny]` at any crate root, so nothing keeps it that way, and the three `useless conversion to
the same type: usize` and three `very complex type` lints are the kind that accumulate into the readability
the module docs are proud of.

One stale claim is worth calling out because it is load-bearing for the design review's conclusion.
`incremental-edits.md:838-840` says:

> `want_the_closure_cooked(root)` marks the **direct includes and the level under them** — it has never
> marked the root.

`cpp_code_analysis/src/session.rs:1394-1396` marks the root:

```rust
if !already_read(self, root) {
    self.cooking.want(root);
}
```

The conclusion §8.7 reaches — that the edited file's rendering is request-driven rather than eager, so
removing the eager cook would move work rather than remove it — survives, because that argument rests on
`analysis_state.rs:167-174`, which is still true. But the sentence a reader will check first is false, and
the line it is about has been there since before the document was last edited.

---

## 5. What this audit would do next, in order

Ordered by (measured user impact) ÷ (cost), with the acceptance each one is judged by. Two of these are
already on the project's own list; they are here because the list's *order* is wrong.

```text
1.  Answer the latency test. `visibility_answers` removable by file via `dependents_of(inserted)`,
    and while that is being written, enforce the budget instead of logging it: check elapsed time
    inside the update closure and hand the remainder back to the pump — the prepare_*/commit_* split
    already permits it. Then slice `read_the_modules`, which holds the write lock for a measured
    6151 ms cold / 547 ms warm (handlers/mod.rs:73-79).
    acceptance: `cargo test -p cpp_ls --test latency` passes, and `worst_write_hold` stays under 50 ms
    for the whole of a cold index. Criterion: the twelve numbers, not the verdict.

2.  Fix `expand.rs:786` and `expand.rs:869-877` — one function, two call sites — and add
    `overflow-checks = true` to a `[profile.release]` so the next one is a panic in the field rather
    than a `SourceRange` covering the file.
    acceptance: `cargo test -p cpp_code_analysis --test expand` green, and the two fixtures
    (`#define A F` / `B (7)`) keep their expansion.

3.  Build the two artifacts clangd, IntelliJ and VS all have: an index reading that never sees a body,
    for the pump, and the whole tree for the file a request names. This is `design-review.md` §3.4 and
    it is the last architectural item on the board — do not re-litigate §3.3 (skipping bodies *inside*
    one tree), which was measured at zero.
    acceptance: `parse` + `sweep` on a cold index falls by the share of body bytes, and no capability
    the open file has today changes.

4.  Stop throwing the reason away. Give `RequestOutcome` a `Reason(UnknownReason)` arm and render it:
    hover as a line of prose, definition as a `window/showMessage` at debug level, completion as a
    server-side-only log plus `isIncomplete`. Extend `isIncomplete` to `semantic_token`,
    `signature_help` and `inlay_hint`, all of which currently answer `null`-equivalent while pending.
    acceptance: a definition on a name in an unread header says *why* over the wire.

5.  Make `references` refuse rather than over-report, or resolve it. The cheap honest version is to
    return the list only when the symbol's qualified name is unique among candidates, and `null`
    otherwise — which is what `Ambiguous` was built for and what `rename` already does.
    acceptance: `references` on `size` in a file including `<vector>` does not list every `size` in
    the standard library, and `status.md`'s claim about the feature becomes true.

6.  Per-capability readiness (IntelliJ `DumbAware`), which is the same change as (5) generalised: a
    table of which handlers may answer on partial data, and a status the client can see. `references`,
    `rename` and `workspace_symbol` already have the refusal; what is missing is that nothing *else*
    degrades and the client is never told.

7.  Incremental sync, code actions, `declaration`/`typeDefinition`/`implementation`/
    `documentHighlight` — the breadth clangd and CLion ship. Ordered last on purpose: none of it makes
    a wrong answer right, and (1) is what makes the features we already have usable.

Never: instantiate template bodies; replace rowan; skip bodies inside one tree; add a second
implementation of any question (the three in §2.3 are the bill for that so far).
```

## 6. Two process findings

### 6.1 The oracle is not automated, and there is no CI

There is no `.github/`, no CI configuration of any kind, no `rust-toolchain.toml`, no `clippy.toml`, and no
`#![deny]`/`#![warn]` attribute at any crate root. Every "green suite" claim in `docs/` is therefore a
claim about one machine on one day, and §2.2 is what that costs: three targets have been failing for an
unknown length of time and two of them are not mentioned in any document.

`align.rs` is a compiler-differential harness that runs as an **example binary invoked by hand**
(`align.rs:59-64` says so, deliberately). The design is right — pure comparison, recorded fixtures, one
impure entry point — and the wiring is missing. A CI job that runs `cargo test --workspace` plus
`examples/align_preprocessor.rs` on one MSVC header set would have caught §2.3(b) and §4's drift on the day
they appeared. **The repository has the best test suite in this review and no way to know whether it
passes.**

### 6.2 Rules the documents have already earned, applied to the documents

`docs/status.md` §6 ends with five rules and they are good ones. Two of them bite the documentation itself:

* **"A green suite you cannot run is not a green suite. Run it, or do not quote it."** The counts at the top
  of §6 should be regenerated by a script or removed. A number that a reader cannot re-derive is a claim,
  and this file's whole value is that its claims are measurements.
* **"A criterion that bites may still bite the wrong shape."** `latency.rs` bites, and its shape is right.
  `expand.rs`'s two tests bite a *region* that the code's own comment says is not a region at all — the
  test is correct and the fixture is what found it.

And one rule worth adding, from §2.3: **when a second implementation of a question appears, one of them must
be deleted in the same commit.** The `expand` defect and the member-vs-name contradiction were both two
spellings of one question, and both were found by reading rather than by a test.

---

## 7. The honest one-paragraph summary

The analysis engine is a serious piece of work: the macro timeline is a real idea, the compiler-oracle
harness is better than what either reference tool exposes, the refusal vocabulary is honoured in the code
rather than in the prose, and the parser's recovery invariants are fuzzed in a way clang's are not. What is
missing at that layer is template deduction — a known, scoped, self-documented gap. The language *server*
around it is where the design has not caught up with the engine: one lock, one artifact, one readiness flag,
one handler that can say "and there may be more", and a 27-second worst case in exactly the window a person
types in. The documents name all of that with more precision than this review can, and then the numbers
drift, because nothing runs them.
