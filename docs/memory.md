# Where the memory is

A session on `EmmyLuaCodeStyle` reports **~500 MB** after it opens, and **~700 MB** after a few files are opened. This
note is the answer to "which part", and it is a measurement rather than a budget: every number below came from
`tests/memory_probe.rs`, which opens a real project, indexes it, opens files the way a reader does, and prints
`Session::memory_report()` at each phase while the driver samples the process's resident set.

```powershell
$env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
cargo test --release -p cpp_code_analysis --test memory_probe -- --ignored --nocapture
# and, in another shell, sample it: Get-Process memory_probe* | Select WorkingSet64
```

The same report is written by the server itself when its index queue drains — `%TEMP%\cpp_ls-memory.txt` — so a
report of "it uses a gigabyte" arrives with the breakdown already attached.

## The phases, measured

```text
                            accounted    with the process tables    resident
Session::open                    0.0 MB            0.0 MB               9 MB
indexed (1590 files)           124.8 MB          124.8 MB             263 MB
+ 5 files opened and cooked    136.6 MB          138.8 MB             299 MB
+ 20 files opened and cooked   173.2 MB          176.7 MB             448 MB   ← after the budgets below
```

The gap between the report and the resident set is **not a leak and it is deliberately not reclaimed**: it is the
allocator's high-water mark from parsing 1590 files (each parse builds a tree, frees it, and the pages stay hot). The
proof is the last row of the earlier run — dropping the session took the process from **448 MB back to 234 MB** — and
the decision is recorded here rather than in code: hot memory is faster to reuse than to ask the kernel for.

## What the report counts, and what it did not

`MemoryReport` counted the session's own tables — the VFS, the summaries, the cooked readings, the definitions, the
units, the renderings — and **nothing of the index's own structure**: the inverted index, the include graph, and the
four memos that outlive a request. That was one of three candidate explanations for a 138 MB gap and there was no way
to tell them apart. It now counts them (`ProjectIndex::table_accounting`):

```text
index tables (EmmyLuaCodeStyle)   12.4 MB
  46k names / 226k name postings, 5 680 scopes / 87k scope postings, 8 314 include edges 1.1 MB
  (and after a pull's checks: 613 memos 2.5 MB)
```

So the index's structure is **12–15 MB**, not a hundred: the remaining gap at the index phase is the allocator's, which
is what the drop measurement says. The report is now trustworthy enough to rank what is left.

It also **over-counted the timelines**, and that was found by measuring rather than by reading: the unit table and the
closure-sharing cache hold the *same* `Arc<TranslationUnit>` whenever a timeline was both held for a root and
remembered for its content, so adding their bytes counted one allocation twice — `units 40.4 MB + walks 46.6 MB` for
what was 46.6 MB of memory. `shared_timeline_bytes` is that overlap, measured by comparing pointers, and the totals
subtract it.

## The budgets, and the measurement behind each

```text
                         was              now      why
MAX_UNITS                 16                8     20 files cooked: 16 timelines = 40.4 MB,
MAX_UNIT_BYTES           128 MB           64 MB   and a second pass over them hit twice
MAX_WALKS_BY_CONTENT     512 / 64 MB      32 / 16 MB   0 hits on two real projects, 46.6 MB held
```

The closure-sharing cache's entire justification was a mass indexer ("2110 roots, 786 distinct closures, 46 % of walks
removable") and **that caller no longer exists** — phase 2 stopped building units for the second pass. Measured on two
real projects, twenty files opened and cooked in each:

```text
EmmyLuaCodeStyle   20 walks, 0 from memory, 46.6 MB held
clice              20 walks, 0 from memory,  0.6 MB held
```

A cache with a zero hit rate that holds 46.6 MB is the failure mode the byte cap exists for; 16 MB keeps what it is
good at (sibling roots that *do* share a closure) and bounds the waste. The unit table is the useful one — it holds a
root's own timeline for its own cooks and pulls — and it was sized for a caller that has since moved; eight is a
reader's working set (the file it is looking at and the headers beside it), and a timeline that falls out is a 38 ms
walk behind a rendering cache that usually answers first.

## What is left, ranked by size

```text
summaries            83.6 MB   1590 files, 199 022 facts, 46 191 macros = ~420 B/fact
vfs                  28.8 MB   23.6 MB of text + 5.2 MB of line indexes, for every file ever read
walks by content     15.0 MB   capped at 16 MB
units                27.2 MB   capped at 8 entries / 64 MB
cooked                0.4 MB   (7.4 MB on the running server, which cooked 163 files)
index tables         15.0 MB
definitions          10.3 MB   30k `#define` bodies, growing with the closures a session has read
```

The first two are the only ones worth a plan, and neither is a tuning constant:

* **`summaries`** is the index's facts and it is the floor of the design — 420 bytes per fact, dominated by the
  `Option<Box<str>>` fields a `DeclFact` carries. Interning the strings would be the real change, and it is a data-model
  change rather than a budget one.
* **`vfs`** keeps the text of every file it has read, which is what makes a view a pointer instead of a read. The
  line indexes (5.2 MB) are derivable from the text; the text itself could be dropped for files that are not open,
  at the cost of a re-read.

Neither is started. The numbers above are the baseline a plan for them would have to move.
