# Executing the performance plan

`docs/perf-review.md` is an outside reading of this server's cold path: a closure walk that scanned a `HashSet`
instead of hashing it, a cold closure computed twice by two callers, an empty diagnostics answer that still paid the
whole gate. This file is the record of what was done about it — **what was measured, on what, and what is still
open**. It is kept apart from the review so that neither document has to be edited to agree with the other: the
review is what was reported, this is what happened.

Every number below is from this machine (Windows, MSVC 14.51 STL, release build) and from a probe in
`crates/cpp_code_analysis/tests/unit_read_probe.rs` or `crates/cpp_parser/tests/parse_cost.rs`, both runnable:

```text
$env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_std"      # 39 standard headers, one 1 KB main.cpp
$env:CPPLS_UNIT_ENTRY   = "main.cpp"
cargo test --release -p cpp_code_analysis --test unit_read_probe -- --ignored --nocapture
```

**A cold run means an empty `.cppls`.** Every measurement deletes it first; a warm cache answers in milliseconds and
would hide the defect being measured.

## 1. The parser read the environment once per token — the largest single cost found

`CppParser::bump` runs once per token, and it contained

```text
assert!(std::env::var_os("CPPLS_TRIVIA").is_none() || !is_trivia_kind(self.current_token), …)
```

`std::env::var_os` is a lock and an allocation, made a million times per program to answer a question that cannot
change during a run. This is the defect `cpp_code_analysis::probes` was written to prevent, in the crate that did
not have the module yet.

| what | before | after | |
|---|---|---|---|
| 4 MB unit, 1 091 827 tokens | 2.090 s — 1.93 MB/s | **0.663 s — 6.07 MB/s** | 3.15× |
| 44 real MSVC headers, 4.8 MB | 2.163 s — 2.21 MB/s | **0.591 s — 8.10 MB/s** | 3.66× |
| `int f(int a);` × 16 000 (no trivia at all) | 417 ms | **118 ms** | 3.5× |

The fix is `crates/cpp_parser/src/probes.rs` — every switch in that crate read once, behind a `OnceLock` — and the
three use sites now call it. End to end, on the 39-header file:

| | before | after |
|---|---|---|
| `index_everything`, 366-file closure | 8.25 s | **3.38 s** |
| `read_the_unit` (whole program as one parse) | 9.06 s | **6.37 s** |
| `render_the_unit` (5.05 MB, 196 files) | 1.09 s | 1.06 s |
| second `read_the_unit` | 216 ms | 125 ms |

Tree building is now **36%** of a parse where it was 13% — the grammar had been dwarfed by its own assertion.

## 2. The linear scans the review found (`Marked::mark_uncertain` / `is_uncertain`)

Both walked a `HashSet` with `iter().any()` for every question. They are membership tests on a hash set, and they
are now `insert` and `contains` — the same answer, one hash instead of a scan.

**Measured effect on the corpora to hand: small, and that is the honest number.** The cost of the scan is the
*size of the uncertain set* times the number of questions, and the size is a property of the corpus:

| corpus | `read_the_unit` before | after |
|---|---|---|
| 39 MSVC headers (366 files, 96 579 facts) | 9.06 s | 8.76 s |
| `index_everything` over that closure | 8.25 s | 7.37 s |

The review measured **23.84 s → 0.32–0.39 s** for the same two lines, because its closure left thousands of macro
facts unplaceable and every condition asked about all of them. Nothing here contradicts that: the fix is
`contains` either way, and the corpus decides what it was worth. What is *not* true is claiming the win on a
corpus that does not show it.

## 3. `qualified_declarations` is 360–400 ms of a program read, and finds nothing in these projects

`build_facts` walks the whole tree a second time to find declarations written with a qualified name
(`void CodeFormat::UpdateCodeStyle(…)`), and then adopted each one into the facts by a **linear scan** of the fact
list. The scan is now a map by offset — quadratic before, one hash per question now.

| corpus | facts | qualified declarations | `qualified` phase |
|---|---|---|---|
| 39 MSVC headers | 31 303 | 0 | 460–495 ms |
| `EmmyLuaCodeStyle/CodeFormat.cpp` | 49 406 | 0 | 396 ms |
| `clice/Selection.cpp` | 45 597 | 1 | 359 ms |

So the map did not change these numbers — the product `Q × F` was ~0 on all three. The cost that *is* there is the
pass's own tree walk, and the two corpora are `.cpp` files whose classes are defined inline in headers. Folding the
pass into the scope walk (which already visits every declaration and knows which ones it refused to bind) is the
next step; it is written down in the comment on the function rather than done, because it changes where facts are
made and needs its own regression run.

`build_facts` after the parser fix, on the 39-header file (5.05 MB, 31 303 facts):

```text
scopes            1 551 ms   the scope tree: every node of 694 000
scopes+facts      1 646 ms   the per-scope binding loop through `fact_for`
qualified           460 ms   the pass above
frees               164 ms
reflection          177 ms
guards                0.3 ms
parse               816 ms
```

## 4. The diagnostics gate is now asked before the rules run, not after

A check reads the *index*, and an index still filling answers absence as a fact, so a finding may only be published
once the file's closure has been read (`Session::is_closure_ready`). The server asked that question **after**
`Session::diagnostics` had run every rule, so a pull on a cold closure paid for the rules and then threw the answer
away. The question is now asked in the session, before `sema::check::run`, and `FileDiagnostics::checks_withheld`
says why `checks` is empty.

Measured with the gate forced open (`let trusted = true`), on a fixture whose closure is cold (`deep.h`'s summary
dropped) and whose text has two findings in it:

```text
unfixed   checks: [Unused "`count` is declared here and never used",
                   AssignTypeCheck "`count` is initialised with a value that does not convert to `int`"]
fixed     checks: [], checks_withheld: true
```

The test is `a_pull_whose_closure_is_unread_withholds_its_checks_and_says_so` in
`crates/cpp_code_analysis/src/session/tests.rs`; it fails with the gate removed and passes with it. On a real file
that is four rules resolving types — the review measured **3.5 s** for zero findings — paid once per pull for a file
whose closure is cold.

## 5. A cold closure is walked once, not once per caller

A miss in the visibility memo is computed **outside** the lock that guards it, which is right for the lock and wrong
for the work: two callers that miss the same entry build the same closure twice. In a server the pair is the pump
prewarming a file the reader has open and the reader's own request arriving — the review measured two concurrent
cold walks of one closure, at ~24 s each.

A walk is now **claimed** in a `HashSet` of the roots being walked before it starts, and released by a `Drop` guard
however it ends (a walk has several exits, and each of them has to wake the waiters). A caller that finds its root
claimed waits on a `Condvar` for that answer instead.

Measured with eight threads released by one barrier on a 40-file cold closure:

```text
without the claim   8 walks for 8 callers   (`walks_met` 0)
with it             1 walk  for 8 callers   (`walks_met` 7, the rest memo hits)
```

The test is `a_cold_closure_is_walked_once_for_everyone_who_asks_at_the_same_time`, and the count is what it
asserts — not a duration. The claim has a 30 s safety valve: a walk that never finishes makes a request slow rather
than wedged.

## 6. The pull memo is keyed by the file and its closure, not by the store's revision

`the_state_of_a_pull` carried `Store::revision()`, which moves on **any** summary insert — so a keystroke in any
file invalidated every other file's findings, and the next pull re-ran every rule (143 ms measured, 97% of it type
inference). What is in the key now:

| input | what it catches |
|---|---|
| the file's own `SummaryKey` | its own content, context and environment |
| `the_closure_fingerprint` | `(path, content_hash)` of **every file the closure reaches** — a macro, an include or a declaration |
| the number of files the index holds | a file added to or removed from the project, which is the one thing `missing-include` reads outside the closure |
| the enabled rules, the file's language, the project's Unreal flag | the configuration |

A closure too large to fingerprint (`None`) puts the revision back in: a key that is too coarse is a slow pull,
one that is too narrow is a wrong finding.

Measured, with the memo's own counters:

```text
                        hits  runs
keystroke outside the closure    1     0     ← was 0 and 1 with the revision in the key
keystroke inside the closure     0     1     ← and it must be: the answer could have changed
```

The test is `a_keystroke_outside_the_closure_does_not_rerun_another_files_pull`; it fails with the revision in the
key and passes without it.

## 7. The counters, and where they are readable

`Session::pull_counts()` → `(hits, runs)`; `ProjectIndex::walk_counts()` → `(built, hit, met)`; the existing
`StoreStats` and `UnitStats` cover parses and program reads. `inspect stats` prints all four groups:

```text
stats: 198 file(s) in the project, summaries reused 9 built 0 not stored 0
       units from memory 0 walked 0
       closure walks built 0 hit 0 met 0
       pulls hit 0 run 0
```

They are **counts, not durations**, which is what lets a regression assert them in a debug build: CI runs
`cargo test --workspace --all-features` on the debug profile, where a latency threshold is noise. The three
assertions that exist are the three numbers above, and each fails on the behaviour rather than on the clock.

## 8. What is still open

- **Background pacing.** Index workers and the cooking queue still compete with an interactive request; nothing in
  this round touched the scheduler. The plan's §4 (limit the background pass, prefer the open file) is the next
  piece.
- **`build_facts` walks the tree five times** — scopes, facts, qualified declarations, frees, reflection — and the
  qualified pass finds ~nothing on the corpora measured (0, 0 and 1 declarations). Folding it into the scope walk is
  the next structural win; see the note on `qualified_declarations`.
- **`read_the_unit` is still 6.3 s** for a 5 MB program after the parser fix (parse 0.8 s, scopes 1.5 s, facts
  3.4 s of which the per-scope `fact_for` loop is 1.6 s). The next measurement is inside `fact_for`.
