# C++23 modules: the indexing plan, and where it stands

A record of the P0–P6 work on header and module indexing, with the numbers each phase was judged by. Every number here
is a measurement from `crates/cpp_code_analysis/tests/` — the probes are `#[ignore]`d and run with `--ignored
--nocapture`; the fixture is an external project at `C:\Users\zc\Desktop\cpp_modules` (`import std;`, 401-file closure),
**not vendored** into this repository, and every probe skips cleanly when it is absent.

The headline: **`import std;` went from 24.7 s cold and 112–506 s warm to 2.5 s cold and 264 ms warm**, with the standard
module's names — `std::format` among them — reachable from a file that does nothing but import it.

The final run of every probe, for the record:

```text
std closure:  388 files, 17.9 MB, discovered in 387 ms (read 35, lex 121, directives 142)
import std;:  264 ms warm (parsed 0, reused 401);  cold 2.5 s with a warm cache, 10.0 s from nothing
unit read:    second 0.10 s, restart 0.11 s — both "served without the walk"
`format`:     offered in the completion; 4 declarations in the index of `std`
workspace:    145 suites, 0 failures
```

## P0 — probes on an external fixture

| probe | what it measures |
|---|---|
| `modules_probe` | the `import std;` read per file, and the `std::` completion |
| `unit_probe` | a program read's prepare/commit split, and the same read after a restart |
| `walk_probe` | the include walk over `std.ixx`, without the parser |
| `closure_probe` | the std closure's discovery: files, bytes, and the phases |
| `filefacts_probe` | discovery's share of a summary |
| `scale_probe` | indexing against project size |

## P1 — facts in the VFS, a lexer-only directive scanner, a persistent include cache

* **The walk no longer parses.** `walk_into` used `CppParser::parse` per file; it now just lexes. Measured on
  `std.ixx`: a 204-file walk went from **37.82 s to 1.05 s (36×)**.
* **The directive scanner is lexer-driven** (`Directives::of`), and `visit` no longer clones the project's macro table per
  directive — that clone was 9 401 entries × every `#include`, and it was where the 185 ms/file went.
* **A resolution is remembered across sessions** (`include_searches`, keyed by spelling, form, `#include_next`, the
  asking directory, the include-path set and the dialect). The std closure is now discovered in **376 ms**, of which
  read 26 ms, lex 121 ms and the directive scan 141 ms — the searches no longer appear.

## P2 — demand-driven summaries

* **Landed.** `index_everything` owes the *index*; a reading of a file as a compiler sees it is owed to whoever asks
  (an open file, a request, the pump). Warm `import std;`: **1.45 s → 360 ms**, and **266 ms** after the walk skip below.
* **Closure memoisation was measured and is not needed**: the closure walk is **1–7 ms** of a 150–580 ms pass.
* **The byte budget is built** (`Session::enforce_the_summary_budget`, 1 GiB, largest summaries first, four kinds of file
  never touched), and it is sized by a measurement: a 401-file std closure holds **52 MB of summaries and 25 MB of cooked
  readings**, so a 100 000-file project would hold **13 GB**. **It cannot fire yet**, and that is the open item: under the
  demand-driven contract the second pass's backlog is never empty, so every indexed file is protected by one of the four
  rules. The resolution is a policy for which *queued* files may be forgotten (the pass's own skips), not a looser filter.

## P3 — parallel building

`prepare_closure`, `prepare_a_wave` and `units_for_the_pass` already build in parallel (`parallel_map`, scoped threads,
a shared counter). What is sequential is the drain's per-file decode and insert, and one program's parse. Open item.

## P4 — a per-project cache in redb

* The sharing cache was folded into the project's redb database (`bfb2a34`); `sharing.bin` is gone.
* **No cross-project sharing**, by test: `a_refinement_belongs_to_the_project_that_built_it` asserts the cache lives in
  the project, and that a second project serving the same file gets **0** refinements served.
* **A rendering records the walk's own file list** (`RenderedUnit::walked`, codec v4), so a stored rendering can
  reproduce the key it is filed under: a `unit_roots` row takes a warm read from the root to the rendering without
  walking the closure. Measured: a fresh process reads the std unit with **prepare 0.11 s** (was 0.18 s), every read
  "served without the walk". What remains is the proof — 223 texts (22 ms) and the decode (~45 ms).

## P5 — the module's names reach an importing file

* **The interface unit is read** as part of `read_the_modules_a_file_imports` (0.2 s), instead of only being queued for
  a pump that a batch caller does not have.
* **A namespace opened inside one of the same name is that namespace reopened.** A unit rendering splices files at their
  include sites, so MSVC's `_STD_BEGIN` came out nested ten deep and `std::format` was filed under a scope no query asks
  for. Collapsed in `ScopeTree::qualified_name_of`.
* Result, measured: `` `format`: offered in the completion, 4 in the index of `std` ``.

## P6 — warm is faster than cold

| | |
|---|---|
| cold, cold cache | 10.0 s (everything rebuilt) |
| cold, warm cache | 2.5 s |
| warm, three runs | **266 ms, 262 ms, 264 ms** (360 ms before the walk skip) |
| database share of a run | 0.04 s of reads, 0.01 s of writes |

Four defects, each measured before and after: a served reading was queued for a full cook; a batch read files as
programs when a timeline was merely *held*; the second pass walked one timeline per candidate (44 walks, 46.8 s); and a
"nothing to repair" mark was written but never read back.

## What is open

1. **The byte budget's trigger.** It is built and cannot fire: the second pass's backlog protects every indexed file, and
   under the demand-driven contract that backlog is never empty. The resolution is a policy for which *queued* files the
   pass would skip anyway, not a looser filter.
2. **P3's sequential tail** — the drain's per-file decode and insert, and one program's parse.
