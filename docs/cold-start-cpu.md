# The CPU after the index settles

**Reported:** "after the language server starts and the index is finished, `cpp_ls` keeps using CPU for a long time —
I suspect a handler that keeps being scheduled."

**Answer, measured:** it is the **second pass** — the re-read of files whose summary was built before the files they
include were in the index. On `E:\EmmyLuaCodeStyle` (1122 files, 1037 indexed) that is **773 files**, drained four at a
time, and each slice of four costs **1–4 seconds**. It is real work, it happens only on a cold cache, and it writes
nothing to the log — which is why it looked like a leak.

Everything below is measured from outside the server (process CPU, the server's own log, and a temporary probe that
was removed afterwards), except where a measurement names the code that produced it.

---

## 1. The shape of it

```
cold cache:  t=  2–25 s   up to 17 s of CPU per 2 s      (the index burst, several cores)
             t= 25–120 s  ~0.3–1.5 s per 2 s             (the second pass, one core)
             t=125 s on   exactly 2.00 s per 2 s, flat     (still the pass: see §3)
warm cache:  4.7 s of CPU in total, then 0 % for 300 s   (three separate runs)
```

One process thread does it — `tid 21080: +10.0 s in 10 s, state Running`, and all 21 other threads `Wait`. So it is a
single busy task, not a runtime-wide problem.

The server's real log while this runs is **silent**: after `the workspace is indexed — the longest an update held the
analysis was 127 ms` there is not one more line for minutes. The line that would report it is `debug!`:

```rust
// crates/cpp_code_analysis/src/session.rs
if re_read > 0 || cooked > 0 {
    log::debug!("index slice tail: re-read {re_read}, cooked {cooked}, write lock held {held} ms …");
}
```

and the server's default level is `info`.

## 2. What it is, exactly

A temporary probe printed every collection in the session once every two seconds while the CPU was pinned:

```
index 1037 | queue 0 (open 0 rest 0 queued 0) | cooking 0 | macro_work 0 | re_read 773 | prefetched 0 | renderings 0
```

`re_read` is `Session::parsed_since_the_last_pass.len()` — the work list of the second pass. Its series over 120 s:

```
352 → 448 → 512 → 672 → 832 (it grows: files are added as they are parsed) → … → 749 → 741 → 733 → 729 → 725 → 721 → 721
```

A second probe, at the two ends of one slice, with timestamps:

```
01:37:58  PROBE claim: 729 waiting, taking 4 (idle true)
01:38:01  PROBE commit: re-read 1, 725 still waiting
01:38:05  PROBE commit: re-read 0, 721 still waiting
```

That is the whole of it: **four files claimed, one to four re-read, 1–4 seconds per claim.** 741 files at that rate is
**eight minutes**; the three minutes the user noticed are the first part of it, before the pass has eaten the backlog.

## 3. Why a re-read exists at all

Two deliberate decisions, and neither is a bug:

* **A file is read before its includes.** `SummaryStore::get` reads one file with the evidence the index has at that
  moment, and on MSVC's STL that is no evidence at all: `<vector>`'s `std` scope is written in `yvals_core.h`'s
  `_STD_BEGIN`, which `<vector>` *includes*. So the scope a file opens through another file's macro body is invisible
  when the file itself is parsed. The second pass repairs exactly that, once its closure is in the index.
* **The summary cache is keyed without an environment, on purpose.** `SummaryStore::index_includes_from` says why:
  *"feeding the macro environment in means the key must name it, which invalidates every stored summary in every
  project."* The second read is the price of that cache being useful at all.

Deleting it is not available, and the suite says so — with the pass's work list never fed, four tests fail and their
names are the specification:

```
a_local_fact_carries_a_namespace_the_file_only_opens_with_a_macro
a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session
a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass
a_project_with_no_build_system_is_read_with_the_compilers_environment
```

The cost per file is not the parse. It is the **unit walk** — the environment a file's own reading is checked against.
`session.rs` records the same thing from the other end: a translation unit is built *for a closure rather than for a
file*, so it is one indivisible step, and the code's own numbers are `1 file is 150 ms, 8 is 1105 ms, a wave of 32 is
2636 ms`. On this project — MSVC and Windows-Kit headers — the per-file cost measured here is 250–1000 ms.

## 4. What was ruled out, and how

| Suspect | How it was ruled out |
|---|---|
| The index pump's busy branch | A probe printed the queue state once a second while `pending > 0`. It printed while the index was filling and then **stopped**, so `pending` did reach zero and the pump was not in its slicing branch. |
| The cooking backlog | `Cooking`'s bookkeeping is consistent (`want` inserts into both the deque and the membership set, `take` removes from both, `len` is the deque's length), so it cannot hold a file it will not hand out. |
| The summary-cache sweep | `cache::prune` on the real cache (1019 files, 56 MB) takes **145 ms**, against a 2 GiB budget it never reaches. |
| The connection reader | `for msg in &connection.receiver` is a blocking iteration; an empty channel costs nothing. |
| The 1-second idle wait in the pump | Skipping it (yield instead of `wait_for_work`) changes nothing: **112 claims in 115 s** against **111 in 125 s** without it. The time is inside the slice, not between slices. |
| Guard evaluation / the walk's memoisation | Earlier rounds: a per-region memo keyed by `(region, branch, environment generation)` is correct but inert on this workload (102.8 s against 103.9 s), and a per-`(file, environment)` cache had only 1.5× reuse to offer (99 238 visits, 67 554 distinct pairs). |

## 5. What is still open

Two things, both measured and neither explained:

* **The pass stops at ~721 and the core keeps burning.** At that moment *every* collection the probe could see is
  static (`index 1037 | queue 0 | cooking 0 | macro_work 0 | prefetched 0 | renderings 0 | re_read 721`) while the CPU
  is still at 100 % of one thread. So whatever burns it touches no session state.
* **Claimed files that are never re-read.** `commit: re-read 0, 721 still waiting` — four files were taken off the work
  list and none was re-read. `commit_the_drain` returns how many were committed; the rest are dropped silently, which
  is a correctness question rather than a speed one.

## 6. Levers, if this is picked up again

1. **Make the closure walk cheap** — that is where every millisecond of the second pass goes, and it is the same walk
   the whole index is built on. This is the subject of `docs/walk-redesign.md`.
2. **Read a file once, with its environment** — the session already has that path (`prepare_with_the_environment`,
   taken when a non-provisional unit for the file is already held) and it is what `Step::with_the_environment` marks.
   Widening it to more files removes re-reads rather than speeding them up, at the cost of the cache key.
3. **Log the pass at `info`.** One line — `index slice tail: re-read N, …` — is the difference between "the server is
   busy" and "the server is doing the second pass", and it is currently invisible at the default level.

## 7. Reproducing it

```
# cold: the project's own cache
Move-Item E:\EmmyLuaCodeStyle\.cppls E:\EmmyLuaCodeStyle\.cppls.backup

# the server, driven by the suite's own answering client (it answers the server's requests)
cargo test --release -p cpp_ls --test handshake probe_the_cpu_shape -- --ignored --nocapture

# while it runs
Get-Process cpp_ls | Select-Object CPU
Get-Process cpp_ls | ForEach-Object { $_.Threads } | Sort-Object TotalProcessorTime -Descending | Select-Object -First 3

# the server's log (its own path, not stderr)
%LOCALAPPDATA%\cpp_ls\logs\E_EmmyLuaCodeStyle.log
```

The probe test is `#[ignore]`d because it drives the real project for four minutes and asserts nothing. A hand-written
LSP client is not enough to reproduce it: the server asks the client for things
(`window/workDoneProgress/create`), and a client that never answers leaves it waiting instead of indexing — measured,
two such runs sat at 5 s of CPU for five minutes.

## 8. Corrections worth keeping

Recorded because each of these sent a search in the wrong direction for a round:

* **`Σstages` is not wall clock.** The instrumented stage totals were summed across threads and compared against wall
  time; they said "walk = 79.8 % of load" and that number was used to choose targets for several rounds.
* **`log::debug!` is not a log.** "The pass writes nothing" was read as "nothing is running"; the pass writes a line
  that the default level drops.
* **The server's log is not its stderr.** `--log-path` and the harness's captured stderr are different channels; one
  measurement of "the log is empty" was taken on the wrong one.
* **A warm cache is not a cold one.** Three warm runs at 0 % CPU were read as "the spin does not reproduce", when they
  were simply runs with an empty work list.
* **"Pinned forever" was an inference.** The pin was observed for 164 s and never for its own end.
## The cold index decomposed, today, on EmmyLuaCodeStyle (cold cache)

Run with the project cache **moved aside**, so every summary is rebuilt:

```text
opened in 223 ms, indexed in 120361 ms   (1122 file(s) in the index, 0 pending)
of which:           111830 ms to index, 1122 file(s) in the index
```

So of today's **120.4 s**: the session opens in **0.2 s**, and **111.8 s** is the index itself — the drain that reads
the project, re-reads the files whose includes arrived late, and cooks. The remaining ~8.4 s is the cooking and check
phase the probe runs after it. That is the coarsest useful split, and it says where the next measurement must be: an
`advance(64)` slab loop around the same drain, printing `pending()` after each slab, would show whether the 111.8 s is
the first pass or the second.

## 和 it: the same run is a false-positive census, and the numbers are not good

The probe prints every finding it makes, and on this project (`LanguageServer.cpp` plus its closure) it made **both** a
decomposition and a list that reads like a defect report:

```text
  **40 findings**: 19 argument-count, 12 assign-type-check, 6 undefined-field, 2 include-not-found, 1 missing-return

  [argument-count]    `NameStyleRule` needs 2 argument(s) and this call passes fewer      (17 of the 19, one header)
                      `CodeActionService` needs 1 … / `dump` needs 4 … / `Run` takes 0 …
  [undefined-field]   `_session` has no member `Send`; `object` has no member `Deserialize`
  [assign-type-check] `_start` is initialised with a value that does not convert to `Position`
  [missing-return]    a template `void` function "has no return"
  [include-not-found] `asio.hpp` — correct: the dependency is not vendored
```

The shape of the argument-count ones is the same defect three times: `_session.Run(...)` and `_session.Send(...)` are
**member calls**, and the check resolves the name `Run` to the free function of that name — the wrong declaration, with
the wrong arity. That is one root cause for 19 + 6 findings, and it is the first thing to fix for "usable".

Real code compiles, so most of those are **wrong**. `argument-count` on a member called on an object (`_session.Send(…)`)
counts only free functions of that name; `undefined-field` is the same class of gap; `missing-return` reads a template's
`void` return as something a body must return. That is the "usable" question that matters more than any performance
number, and it is now a recorded census rather than an anecdote: **the fixes to make next are the checks' false
positives, not more speed.**
## The census, one round later: 40 → 37, and the two defects it exposed instead

The member-call fix (`argument_count` skips a callee that is a member access, because resolving the member needs the
object's type) removed **3** findings and no test depended on them. Pinned by
`checks.rs::a_member_call_is_not_judged_against_a_free_function_of_the_same_name`.

The remaining 16 `argument-count` findings turned out **not** to be member calls, and reading two of them was worth more
than the fix:

```text
LuaDiagnosticStyle.h has **53 lines**; the findings cite lines 436, 541, 640, …, 1764
the flagged call is `NameStyleRule(NameStyleType::SnakeCase)` — **one** argument — and the message says
"needs 2 argument(s) and this call passes fewer"
```

**The first of those two was this probe's bug, not the server's** — corrected here rather than left standing, because a
wrong record is worse than a missing one: the probe printed `finding.range.start_offset` in a `path:line` field, so a
53-line header's **1764th byte** was written down as its "line 1764". The range is a byte offset of the *rendering* the
check ran over, and that file is 1764 bytes long. The probe now prints the line, and the findings read plausibly
(`LanguageServer.cpp:11` is the `#include "asio.hpp"`).

**What the readable positions then showed** — two classes of false positive, both confirmed by reading the code they
point at:

```text
1  **The published line is not a line of the file.** A 53-line file cannot have a diagnostic on line 1764, so the
   range survives a coordinate change (`ReadingOffset` vs `FileOffset`) somewhere on the publishing path. Every
   diagnostic on a file whose rendering differs from its text is then drawn in the wrong place — which is worse than
   a missing diagnostic.
2  A call whose name is also a **class/constructor** is resolved to a two-parameter function of the same name, so the
   arity is judged against a declaration the program never called.
```

```text
LanguageServer.cpp:60   `_session->Send(std::move(message));`     `_session` is a `std::shared_ptr<Session>`:
                                                                  `members_of("std::shared_ptr")` is `shared_ptr`'s
                                                                  member list, the member the program means is behind
                                                                  `operator->`, and the check reports and does not
                                                                  follow it
LSP.h:29                `array.push_back(v.Serialize());`          inside `if constexpr` in a template: the receiver's
                                                                  type is not knowable there, and the check reports
                                                                  anyway
```

The next fix is the arrow one and it is small: **an arrow access is not evidence** — the pointee's type is a question
this check does not ask, so when the receiver's type is a known class and the member is not in its list, an `->` should
be silence. That needs `MemberAccess` to say which access it was (`resolve::member_access_of` knows, and the struct does
not carry it), which is why it is the next round's work rather than this one's.
## The arrow guard: 37 → 33 findings, and the census is now an instrument

`MemberAccess` now says which operator it was (`.` or `->`), and `undefined-field` treats an arrow as **not evidence**:
the pointee's members are not that check's question, and `std::shared_ptr<Session>` has no `Send` in its own list. Pinned
by `checks.rs::a_member_reached_through_an_arrow_is_not_judged_as_missing`, which also asserts that a `.` on a class
without the member is **still** reported — the guard cannot become a blanket silence.

Measured on EmmyLuaCodeStyle with the same probe and the same entry file:

```text
before this round   37 findings   19 argument-count  12 assign-type-check  6 undefined-field  2 include-not-found
after                33 findings   16 argument-count  12 assign-type-check  **2** undefined-field  2 include-not-found
```

The two that remain are `array.push_back(…)` inside an `if constexpr` in a template — a receiver whose type is not
knowable there, which is the next class and a different fix.

**Why this is worth more than it looks:** the census is now a **two-minute instrument** (`checks_probe` with a warm
cache, 33 findings on this project, positions that are real lines), so "usable" has a number that moves instead of an
argument. The order of the remaining work is visible in it: 16 `argument-count` (a class-or-constructor call resolved to
a same-named function) then 12 `assign-type-check`.
## The type-name guard: 33 → 17, and `argument-count` is quiet on this project

A call whose name is **also a type** is a constructor call. Which constructor it means is overload resolution among
constructors, which this check does not do — so the check now says nothing about it rather than judging the call against
a same-named *function* fact's parameters. Asked before **both** ways of finding a callee, because a function defined in
the same file takes the first path and the index's facts are never consulted for it (the first version of the guard sat
in the index path, and the fixture that reproduces the defect passed straight through it — the test caught that).

```text
census, same probe, same entry file:
  round 4   40 findings   19 argument-count
  round 5   37           16
  round 6   33           16   (the arrow guard: undefined-field 6 → 2)
  round 7   **17**       **0** argument-count
```

What 17 is made of now:

```text
12  assign-type-check    `_start` initialised with a value that does not convert to `Position` — a class-to-class
                         conversion this layer cannot see, which is the next class
 2  undefined-field      `array.push_back(…)` inside `if constexpr` in a template: no knowable receiver type
 2  include-not-found    `asio.hpp` twice — **correct**: the dependency is not vendored
 1  missing-return       a template whose return type is `void`-returning, read as needing a `return`
```

The number moved 40 → 17 in four rounds with a test behind each move, and it is now a measurement anyone can repeat in
two minutes: `checks_probe` on a warm cache, count the `[code]` lines.
## Two guards on `assign-type-check`: 17 → 5 findings, and 2 of the 5 are correct

All twelve of the project's `assign-type-check` findings had one of two shapes, and neither is a conversion this rule can
judge:

```cpp
LineSpace line_space_after_if_statement = LineSpace(LineSpaceType::Keep);   // ten of them
Range(Position _start = Position(0, 0), Position _end = Position(0, 0));    // two of them
```

```text
a construction        the initialiser is a call to the type's **own name**; what the type engine answers for that call
                      is not the type it constructs, so there is nothing to compare — `constructs_the_written_type`
a default argument    the initialiser is a parameter's default; what a default may be is the parameter's declaration,
                      not a variable's own type — the loop now skips an initialiser inside a `ParameterList`
```

Pinned by `checks.rs::a_construction_and_a_default_argument_are_not_conversions_to_judge`, whose third half asserts that
`int count = "four"` is **still** reported — and whose first attempt failed, because it used a *class* target with a
string, where `convertible_to` answers `Unknown` and the check is correctly quiet. The fixture was wrong, not the guard.

The census, same probe and same entry file, five rounds:

```text
40 → 37 → 33 → 17 → **5**

what the 5 are:
  2  include-not-found   `asio.hpp` twice — **correct**: the dependency is not vendored
  2  undefined-field     `array.push_back(…)` inside `if constexpr` in a template — no knowable receiver type
  1  missing-return      a template whose return type is a `void`-returning of something, read as needing a `return`
```

So the project went from **38 findings that are wrong** to **3**, measured with a two-minute instrument and a test behind
each move.
## The template-header guard: 5 → 4, and 2 of the 4 are correct

`missing-return` compared the written return type to `"void"` exactly, and a member function template's fact carries
`template<ServiceClass Service, typename... ARGS> void` — the header and the return type in one string — so a `void`
function was read as returning something. Two guards, both about what a spelling *is*:

```text
a spelling containing `template`   not a return type this rule can judge — silence
`void` with a suffix              `void [[nodiscard]]` is still `void`
```

Pinned by `checks.rs::a_function_templates_type_that_carries_its_header_is_not_judged`, whose second half keeps
`int f() { }` a finding.

The census, six rounds, same probe and same entry file:

```text
40 → 37 → 33 → 17 → 5 → **4**

what the 4 are:
  2  include-not-found   `asio.hpp` twice — **correct**: the dependency is not vendored
  2  undefined-field     `array.push_back(…)` where `array` is an `nlohmann::json`: `json::push_back` exists, so the
                         class's member list the index holds is **partial** — a template class read through its own
                         declaration is the next class of defect, and it is not a guard
```

So on this project: **38 wrong findings at the start of the census, 2 now**, with a test behind each move and a
two-minute instrument to repeat it.
## Where the 111.8 s actually is: the toolchain's headers, and the deep ones

`checks_probe --chunked` drains the queue **one step at a time** and reports the distribution. Cold cache, same project:

```text
1122 step(s) in 156270 ms: mean 139 ms, median 13 ms, p90 396 ms, max 8640 ms
the ten heaviest steps are 42712 ms of 155722 ms (27%)

by tree:
   14627 ms over   624 step(s)   the project and its own headers      (23 ms/step)
  141095 ms over   498 step(s)   toolchain (Program Files)            (**283 ms/step**)

by depth:
  depth 0-6    24.5 s over 804 steps    (~30 ms/step)
  depth 7-16  118.5 s over 190 steps    (~623 ms/step)
  depth 12-15  73.4 s over  76 steps    (~966 ms/step)
```

**90% of the cold index is in headers the toolchain owns**, and the heaviest single steps are named:

```text
8528 ms  arm_neon.h            5015 ms  arm64_neon.h        4174 ms  zmmintrin.h
6081 ms  windows.ui.composition.h       4707 ms  windows.media.capture.h
3844 ms  windows.media.core.h           3652 ms  windows.devices.sensors.h
```

### What that licenses, and what it does not — measured, not assumed

The tempting reading is "stop indexing headers the compilation cannot include". It is **unsound for most of them**:

```text
arm_neon.h / arm64_neon.h   included from `<intrin.h>` under `#if defined(_M_ARM…)` — on an x64 build they are
                            genuinely dead, and they are 13.5 s of the 156. Dropping them is precision-preserving.
zmmintrin.h                 x64 **AVX-512** intrinsics: an x64 build includes it. Not droppable.
windows.ui.* / media.*      `windows.h` includes them under `WINAPI_FAMILY_PARTITION(WINAPI_PARTITION_APP)`, and a
                            desktop app *is* in `WINAPI_PARTITION_APP`. Not droppable.
```

The general rule fails for a reason worth stating: an include's guard is decided against the **including file's**
environment, which is the walk's answer — so the queue cannot decide it without becoming the walk.

### The three sound avenues, in the order the numbers argue for

```text
1  priority, not exclusion   the queue currently mixes the project's 624 steps with the toolchain's 498. Ordering it
                             by "the open file's closure, then the project, then the toolchain" turns 111 s of "before
                             you can ask anything" into "in the background", and changes **no** answer. This is the same
                             arrangement the request gates already got (docs/usability.md).
2  lazy units                a deep step is a translation unit over a big closure (`Built` in every heavy step). If a
                             unit is built only when a cooked reading is asked for, the index stops paying for units
                             nobody reads — but the second pass may need them, so this needs its own count first.
3  parallel unit walks       141 s of CPU-bound work on a 16-core machine, at 1.2 cores. Legitimate engineering, and
                             the last step rather than the first.
```
## Avenue 1 was tried and reverted: the order is not free

The change: a third queue tier (`Priority::Outside`) and routing every include outside the workspace root into it, so the
project's own files are read before `<windows.h>`. It compiled, the semantics were unchanged in principle — and the
**chunked probe never finished**: 20+ minutes against the same run's **157 s** before the change, and it had to be killed.

The cause is **not established**, and two mechanisms are consistent with what the code says:

```text
a `pending()` that never reaches 0    `Work::pop` skips an entry whose standing disagrees with the deque it came from
                                      (the upgrade path writes a new entry and leaves the old one), and the skip does
                                      not touch `queued`. A downgrade like `Rest` -> `Outside` is refused outright
                                      (`Some(_) => false`), so the old entry stays — which should be popped normally.
                                      This needs a test on `Work`, not a reading.
a much larger second pass             the re-read slice's cost is measured, in the code's own comment, as 150 ms for one
                                      file, 1105 for eight, 2636 for thirty-two — the pass scans the project's `#define`s
                                      for the names each slice's texts mention. A different interleaving changes which
                                      slices exist.
```

Reverted by hand: the enum variant, the deque, the three queue arms and the include routing. The probe's
`the project and its own files are all read by T ms` line is kept, because it is the right instrument for whoever tries
this again — with a **step cap** in the drain loop, which is what would have made the failure visible in a minute
instead of twenty.

**The lesson, stated plainly:** reordering a queue that a loop drains until empty is a change to that loop's
termination, and the instrument must have a bound before the change is measured, not after.
## The measurement that changed the target: `summarize` is superlinear in facts

Reading `StepOutcome` again — after writing the paragraph above — showed that `Built` means "the file was read and
summarised", not "a translation unit was built". That sent the investigation to `index::summarize`, which a probe can
time directly (`checks_probe --summarize <file>`, outside the library, no instrumentation):

```text
file                                        lines     bytes     summarize    per line      per fact
arm_neon.h                                  7,728    570 KB    8,257 ms     1,068 us      708 us   (11,665)
windows.ui.composition.h                   38,841  1,848 KB    5,302 ms       136 us      572 us   ( 9,262)
LanguageServer.cpp                            106     2.7 KB        0.9 ms       8.6 us        —      (     3)
LuaStyle.h                                    174     5.0 KB        4.5 ms      25.6 us       64 us   (    70)
```

Two facts fall out of it, and they are the point:

1. **It is not per byte.** 570 KB costs 8.3 s while 1.85 MB costs 5.3 s — the *smaller* file is the slower one.
2. **It is per fact, and it grows.** ~700 us per declaration/macro at eleven thousand of them, against 64 us at
   seventy. That is not a constant factor: it is a step whose cost depends on how much has already been collected.

So the cold start's 141 s of toolchain headers is not "too many headers"; it is a **quadratic summary build** on files
with many facts — `arm_neon.h` alone is 5% of the whole cold start because of it. The next step is to cut `summarize`
into its phases with a temporary, gated timer (parse / declarations / macros), find the arm that is quadratic, and
delete the timer after measuring — the same rule the earlier stage timers were removed under.
## The quadratic, found and fixed: `names_a_parameter` inside the visitor

The bisection, each step measured rather than argued:

```text
summarize              8.3 s
  parse                 40 ms   cheap — the tree is not the problem
  index_tree           ~8.3 s
    preprocess          14 ms
    scopes              30 ms
    facts              ~8.5 s
      DeclarationShapes::of   12 ms   cheap — the shape table is not the problem
      the binding walk       ~8.5 s
        per-binding timing:  Function 0.004 ms     Variable **1.9 ms**
        inside the Variable path: using_alias_target 2 ms total, type_of_declaration 5 ms total
```

So the cost was in `declared_type_of_with`, on the `Variable` path, between the two cheap calls — and it was one line
on the wrong side of a closure boundary:

```rust
// before — asked once per *shape on the path*:
shapes.on_the_path(offset, |shape| {
    let name_is_a_parameter = shapes.names_a_parameter(binding.name_range.start_offset);   // ← the scan
    …
});

// after — asked once per binding:
let name_is_a_parameter = shapes.names_a_parameter(binding.name_range.start_offset);
shapes.on_the_path(offset, |shape| { … });
```

`names_a_parameter` is a scan, and the answer depends only on the offset, not on which shape is being visited. A
parameter binding sits under several shapes, so it paid the scan several times — 1.9 ms per `Variable`, 8.4 s of an
8.5 s build.

Measured after the change:

```text
arm_neon.h                8,257 ms → **2,200 ms**   (3.8×)
windows.ui.composition.h  5,302 ms → **2,462 ms**   (2.2×)
LanguageServer.cpp            0.9 ms →     1.1 ms   (noise)
LuaStyle.h                    4.5 ms →     2.3 ms
```

And the guard that matters for precision: **the full suite, 54 targets, 0 failures** — every summary content test, the
census tests, and the latency test. The hoist changes when the answer is computed, not what it is.

What is left of the 2.2 s is the next question, and the instrumented blocks that found this are still in the tree behind
`CPPLS_SUMMARY_TRACE` — they are deleted when that question is answered, not before.
## The same cold run, before and after the hoist

```text
                        before            after
total (chunked)         156,270 ms        116,547 ms      −25%
toolchain (498 steps)   141,095 ms        101,242 ms      −28%
the project's own        14,627 ms         14,738 ms      unchanged (as it should be)
heaviest single step      8,640 ms          3,022 ms      −65%
the ten heaviest steps        27%               18%      the cost is now spread
mean per step               139 ms            103 ms
median                       13 ms             13 ms
```

**The fix is real and it is bounded.** One line moved out of a visitor; 40 seconds off a cold index; every test still
green (54 targets, 0 failures); the project's own files unchanged to the millisecond.

What is left is still the same class of question, one level down: a toolchain step is now ~203 ms of a 498-step 101 s.
Inside `declared_type_of_with`, the two leaf calls are cheap (`using_alias_target` 1 ms, `type_of_declaration` 5 ms for
the whole file) and `on_the_path` is a binary search per level, so the remaining ~0.3 ms per `Variable` binding is in the
visitor's **per-shape work** — which is the next thing to time, not the next thing to guess.
## The second fix: `names_a_parameter` was a descent from the root, once per binding

The same instrument, one level deeper. Inside `declared_type_of_with` the walk was cheap and the two leaf calls were
cheap; the cost was in the one question that went **back to the tree**:

```text
[facts: names_a_parameter 2032 ms]   [facts: on_the_path 3 ms]
[facts: using_alias_target 0 ms]     [facts: type_of_declaration 4 ms]
```

`names_a_parameter` did `token_at(&self.root, offset)` — a descent that scans siblings at every level — and it was asked
once per binding: 6,703 × 0.3 ms = 2.0 s on `arm_neon.h`. The struct's own comment said why the tree was kept at all:
*"kept for the one question that is about an offset rather than about a shape"*. So the answer moved into the table:

```rust
// in `DeclarationShapes::of`, recorded before any skip (a ParameterList is not a shape):
// nested lists are folded into the outer one, so the table is disjoint and start-ordered
if CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ParameterList {
    match parameter_lists.last_mut() {
        Some(last) if last.start_offset <= at.start_offset && at.end_offset() <= last.end_offset() => {}
        _ => parameter_lists.push(at),
    }
}

// the query: one binary search and one containment test
let at = self.parameter_lists.partition_point(|range| range.start_offset <= offset);
let Some(index) = at.checked_sub(1) else { return false };
offset < self.parameter_lists[index].end_offset()
```

Measured, and the guard:

```text
arm_neon.h                 8,257 ms → 2,200 ms → **131 ms**   (63× overall)
windows.ui.composition.h   5,302 ms → 2,462 ms →   276 ms    (19× overall)
full suite                     54 targets, **0 failures** — every summary-content test, the census, the latency test
```

And the cold index over the real project, three runs of the same command:

```text
                        as found    after fix 1    after fix 2
total (chunked)        156,270 ms   116,547 ms     **89,035 ms**   −43%
toolchain (498 steps)  141,095 ms   101,242 ms      **74,082 ms**   −47%
the project's own       14,627 ms    14,738 ms         14,398 ms   unchanged
the heaviest step        8,640 ms     3,022 ms          1,829 ms   −79%
the ten heaviest             27%          18%              10%      the cost is spread
mean per step              139 ms       103 ms             79 ms
```

Two one-line moves — an answer hoisted out of a visitor, an answer precomputed into a table — took **67 seconds** off a
cold index with no change to what any of them computes.

**What is left**: 74 s over 498 toolchain steps is 149 ms per step, and the next question is what a step spends that on
now. The instrumented blocks that found both of these are still behind `CPPLS_SUMMARY_TRACE`; they are removed by
`git diff` as the first task of the next session, because that diff is the only trustworthy list of what was added.
