# A review of the indexing design, measured

This is a review of `docs/indexing-design.md` **against the tree as it stands at `3c2f784`**, written after running the
probes on two real projects and comparing the tree with itself before the indexing phases landed (`f3e2d67`).

The short version: the startup claim is real and large (**26.3 s → 4.6 s** for the whole project, a 5.7× win), and it was
paid for with a capability the corpus measures (**C++ type answers 51/52 → 41/52**), ten red tests of which three are not
what the design note calls them, and one panic on real code. The remaining performance problem is no longer in the index;
it is in the request path, which the design does not discuss.

## 1. What was measured

Two projects, release builds, the pump's own shape (`docs/indexing-design.md` §5's probes, plus `tests/start_shape.rs`
added for this review):

| what | `E:\EmmyLuaCodeStyle` (103 project files) | `E:\clice` (3 361 project files) |
| --- | --- | --- |
| `Session::open`, cold / warm | 25 ms / 26 ms | 270 ms / 155 ms |
| whole-project drain, cold | **4.6 s** (25 slices) | **7.1 s** (89 slices) |
| whole-project drain, warm | 4.7 s | 1.7 s |
| of which prepare / commit | — / — | 6 670 ms (read lock) / 384 ms (write lock) |
| worst single write-lock hold | 43 ms | 24 ms |
| first request: didOpen → catch_up → cook → view | 0.2 + 38 + 6 + 1.7 ms | 0.3 + **237** + 5 + 0.03 ms (cold: 2 402 ms) |
| steady hover (`view`) | 1.3–2.4 ms | 0.02 ms |
| steady `textDocument/diagnostic` | **389 ms** median, 612 ms max | — (its includes do not resolve, see §3.5) |
| C++ corpus, `tests/cpp_corpus.rs` | **41/52 types, 9/10 members** | — |

And the same probes on `f3e2d67`, the tree before phases 0–3:

| what | `f3e2d67` | `3c2f784` (now) |
| --- | --- | --- |
| whole-project drain, cold | 26.3 s (124 slices) | **4.6 s** (25 slices) |
| whole-project drain, warm | 26.1 s | **4.7 s** |
| C++ corpus | **51/52 types, 10/10 members** | **41/52 types, 9/10 members** |
| `include/techniques.h` cooked? | yes | **no** (`NOT COOKED`) |

## 2. What the design gets right

1. **The read/write split is the right shape and the numbers prove it.** Parsing under the read lock and committing under
   the writer keeps the worst write hold at **24–43 ms** on both projects, against the 84–2 430 ms the design note quotes
   from the old single-call `advance`. That is the number a user feels, and it is now bounded.
2. **"A start refines nothing" is the right contract.** A start now costs `Session::open` plus whatever the reader touches,
   and the whole project is read in the background at 5.7× the old speed. The old promise ("the whole project is exact at
   startup") was never affordable in a header-heavy language.
3. **Composition instead of a closure parse is the right direction** — 118–191 ms → 25.8 ms for one refinement is a real
   measurement, and the lazy `MacroBodies` resolver is the right mechanism.
4. **The probe discipline is exceptional.** `pump_probe`, `unit_read_probe`, `keystroke_probe`, the trace switches, and a
   design note whose every claim carries a number is how this kind of work should be done. It is also why this review could
   be written at all.
5. **The layering (L1/L2/L3, one cache, consulted-definition invalidation) is sound in principle.** Nothing in the failures
   below argues against it; they argue that the migration was left half-finished.

## 3. Where it is wrong or risky — in order of what it costs

### 3.1 Phase 2 removed the unit walk, and with it the ability to cook a header

`cooking_materials` ends with

```rust
unit.environment_of(&path)?;      // session/mod.rs:6227
```

— a **held translation unit is mandatory** for a cook. Phase 2 (`b010584`) replaced `let units = self.units_for_the_pass(&slice);`
with `let units = Vec::new();` and removed the fallback. The result is that cooking a header now depends on *some other
file's timeline happening to sit in the unit cache*, i.e. **correctness depends on cache residency**, which is precisely the
coupling the design set out to remove ("a rendering is not a first-class value", §1.2).

Measured: on the C++ corpus the header is `NOT COOKED`, and the corpus goes **51/52 → 41/52** type answers and **10/10 →
9/10** member answers. The failing test that names the capability is
`a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` (`Widget` → `Unknown(NotDeclaredHere("std::Widget"))`).

This is the one thing I would fix before anything else in this document: the composition path (L2 resolved facts, the
`MacroBodies` resolver) needs to be what `cook` uses too — a file's environment must be *composable*, not *cache-resident*.

### 3.2 Ten red tests, and the design note mis-describes three of them

`cargo test -p cpp_code_analysis --lib` is red at `HEAD`: **728 passed, 10 failed**, and they are exactly the ten listed in
§4.8 of the design note. The note's framing is "they assert that a start refines every file, which phase 0 removed on
purpose". Three of them are not that:

* `a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` — the capability of §3.1, not an eager-start
  assumption;
* `a_file_read_as_a_program_needs_no_guard_filter` — an answer changed: `Dead`/`gone`, declared in a `#if` branch the
  compilation does not take, now answer `unknown:ConditionalCompilation` where they used to be found. That is a *semantic*
  difference, and it is not obviously the better one;
* `a_rendering_is_not_left_in_the_cache_for_the_next_session` — the note itself calls this one of **two correctness guards
  that must keep passing** ("they are the reason `env_hash` exists in the key"). It does not pass: the next session's summary
  of the header holds **two** `Live` facts, one per `#if` branch. Keeping both branches may be the new contract (L1 is
  environment-free), but then the guard's *intent* — no rendering leaks from one session to the next — has no test at all.

Rewriting all ten "to the new contract" as §4.8 proposes would delete the tests for the two capabilities above.

### 3.3 The request path is not in the design, and it is now the slow part

The design's remaining phases (4–9) all target the index and the pump, whose numbers are now good (4.6 s for the whole
project, 24–43 ms write holds). The expensive thing a **user** feels is the pull:

```text
steady textDocument/diagnostic on EmmyLuaCodeStyle/CodeFormat/src/CodeFormat.cpp
  all checks                 389 ms median, 612 ms max
  with CPPLS_NO_INFERENCE=1   11.4 ms
  ⇒ 97 % of a pull is type inference, recomputed on every pull
```

`UndefinedField` (316 ms) and `ParamTypeCheck` (222 ms) are the two rules that pay it. The `SemanticModel`'s memo lives for
one request ("the model **is** the request"), so an unchanged file re-derives every answer on every keystroke debounce. And
the whole pull runs under the session **read lock** (`AnalysisState::run_blocking`, `analysis_state.rs:499–510`), so a
389 ms pull is also a 389 ms barrier for the pump's next commit.

A cache keyed by `(content_hash, context_hash, env_hash)` over the *inference answers* — not just over summaries and
renderings — is the missing piece, and it is the one item in this document with a >5× payoff on the number users feel.

### 3.4 The lock is still the architecture, and phase 3 is the largest item for the smallest measured gain

`AnalysisState` is one `RwLock<Option<Session>>` plus a tokio gate, and `Session` carries **15 `std::sync::Mutex`es** behind
it (`session/mod.rs`). Phase 3's `Arc<Published>` is correctly identified as "the largest change in this document" (§4.3) —
but on the numbers above it buys the difference between a 389 ms pull and a 389 ms pull. **Do it after the query cache, not
before**: with the cache in place the published value is mostly immutable data, which is what makes the refactor tractable
rather than speculative.

### 3.5 A panic on real code, and the measurement that found it

Indexing `E:\clice` panicked:

```text
thread 'cppls-index' panicked at sema/declarations.rs:1707:27:
start byte index 37747 is not a char boundary; it is inside '×' (bytes 37746..37748)
```

`rfind` answers a **byte** offset and the code added 1, which is the next character only for a one-byte character. Fixed in
`3c2f784` with a test that reproduces the panic on the old expression. The point for this review is not the bug but the
**class**: nothing in the suite had a fixture with a non-ASCII character before an annotation, and no probe had ever indexed
a project with one. The probes are good; their *inputs* are narrow (two projects, both mostly ASCII, one without include
paths).

### 3.6 The design note's numbers no longer describe this tree

`EmmyLuaCodeStyle` now scans **103 project files**, not the 1 243 the note measures, and a start is 25 ms, not 99 ms; the
note's acceptance targets ("warm `Session::open` ≤ 20 ms", "first request ≤ 50 ms", "cold first pass ≤ 30 s") have partly been
met and partly measure a different scenario. That is not a flaw in the design — it is a flaw in the document as a *plan*:
a reader cannot tell which of §4's targets still matter. Re-baseline the table in §0 and §3 with today's numbers before
starting §4.

## 4. What I would do next, in this order

1. **Make `cook` compositional** (§3.1) — the environment of a file must not require a cached timeline. Acceptance: the C++
   corpus back to 51/52; `a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` green.
2. **Green the suite, honestly** (§3.2): rewrite the seven tests that really do encode the eager contract; *fix* the
   semantic one and *keep* the refinement guard (with a filter-aware assertion). Acceptance: `cargo test -p cpp_code_analysis
   --lib` is 738/738.
3. **Cache inference per (file, content, env)** (§3.3). Acceptance: steady pull on `CodeFormat.cpp` ≤ 50 ms; two consecutive
   pulls of an unchanged file differ by less than 20 %; `Inferred`/`reused` counters printed by the model show a hit rate.
4. **Then** phase 4/5 (one cache, consulted-definition invalidation) — they are what makes item 3's cache *correct* under
   edits, so they are not optional, just later.
5. **Phase 3's snapshot publication** last, when the query path no longer holds the lock for 389 ms.
6. **Widen the probe inputs**: a project with non-ASCII text, one with an include path that resolves, one with modules.
   Acceptance: no panic, and the first two projects in the table above plus these three in every measurement.

## 5. How each of these is verified

The probes already exist or were added with this review:

```powershell
# the pump's shape, the drain, the write-lock hold, the first request
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_code_analysis --test start_shape -- --ignored --nocapture

# the whole-project drain the design note's §0 table measures
cargo test --release -p cpp_code_analysis --test pump_probe -- --ignored --nocapture

# the capability this review says was lost
cargo test --release -p cpp_code_analysis --test cpp_corpus -- --ignored --nocapture

# the pull, and what it is made of
$env:CPPLS_TIME_CHECKS = "1"          # one line per rule per file
$env:CPPLS_NO_INFERENCE = "1"         # the same pull with the engine answering Unknown
```

Every claim in §3 has a command above it in §1, and every item in §4 has a number it is accepted by.

## 6. What has been fixed since this review was written, and what is now coupled

Landed in `c7aac67` plus this round's work:

* **`want_refinement` refused every ask that mattered** (§3.2's first half). It recorded a file only while the file was
  still in `parsed_since_the_last_pass`, and the pump drains that before a request arrives — so in a running server **no
  summary was ever refined**. Any file is now recorded; `is_refined` is the one no-op.
* **The candidate filter dropped every candidate when there were no timelines.** `candidates.retain(… bodies_of(…)…)` is
  about the *units*; with phase 2 building none, `bodies_of` answered `None` for every file, `bodied` came out empty and the
  pass reported `0 candidate file(s) … nothing bodied` — a no-op that looked like a decision. Measured: after this line was
  guarded on `!environments.is_empty()`, `a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` **passes**.
* **`cook` required a held translation unit** (§3.1). The starting macro state is now `Over(configuration, seed)`, with the
  configuration composed from the index when no unit covers the file, so `include/techniques.h` is cooked rather than
  `NOT COOKED`. The walk is still preferred where one exists.

```text
cargo test -p cpp_code_analysis --lib    728 passed / 10 failed  →  734 passed / 4 failed / 2 ignored
```

**The four that remain are one coupled defect, and it is now reproducible in a tiny fixture:**

```text
the pass refines a fixture's own file
  c_files_are_not_told_to_write_cpp_style   `UseNullptr` stops firing — the refined reading of the file
                                            makes every check decline
  a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session
  a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass
  a_keystroke_in_a_body_reads_the_file_through_its_timeline
  a_refinement_is_served_to_the_next_session
```

Refining is what makes those four pass (`index_everything` has to ask — a batch caller is naming every file), and refining
is what makes the three `checks` fixtures fail. So the next measurement is **what the refined reading of a single file looks
like next to the raw one**, in that fixture — the same question the C++ corpus asks at 41/52, and the same one the design's §2.1
answers with "L1 is environment-free; L2 is per (file, environment)": the refined summary is being used as if it were the
environment-free one.
