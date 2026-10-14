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
