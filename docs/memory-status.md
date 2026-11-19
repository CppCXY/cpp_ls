# Memory: where it stands, and what the next step is

Written at the end of the round that fixed the operator/code-lens defect, shrank `DeclFact` again, and put a byte
budget on the pump's read-ahead. Every number here was measured on `EmmyLuaCodeStyle` (1 761 files of include closure
over 13.5 MB of its own text) unless it says otherwise.

## What the memory is, measured

The session's steady state after indexing, before the read-ahead is bounded:

```text
summaries      113 MB   (486 783 declaration facts, 149 164 macro facts, 11 597 include facts)
vfs text        80 MB   + 14 MB of line indexes
index tables    38 MB   (names 24, scopes 6, definitions 13 …)
units           19 MB   (8 whole-program reads, capped at 8 / 64 MB)
cooked          15 MB
walks           14 MB   (by content)
read-ahead     ~183 MB  (prepared-but-uncommitted slices; this is what the budget below targets)
```

`DeclFact` is 65 % of the summary bytes and is where the last two rounds went:

```text
                              before   after
size_of::<DeclFact>()         424 B    160 B
bytes a declaration           508      244
summaries                     236 MB   113 MB
```

The three steps, in order of what they were worth:

1. `name`, `scope`, `type_of` → `SmolStr` (418 777 names of 486 783 are ≤22 bytes and therefore never reach the
   allocator; 68 006 still do).
2. The ten *rare* fields (`parameters`, `bases`, `constraints`, `parameter_defaults`, `parameter_list`,
   `requires_clause`, `pattern`, `returns_in_the_class`, `in_namespace`, `reflection`) are one
   `Option<Box<DeclDetail>>` behind accessors. 424 → 168 bytes.
3. `frees: u64` → `u16` (it is a bitmask of parameter indices) and `returns: String` → `SmolStr`. 168 → 160.

**What is left in the struct is not compressible by field width.** Every field that is a small closed set is already
an enum (`DeclKind`, `Access`, `MacroKind`, `MacroBody` — a fieldless enum — `IncludeForm`, `FactGuard`,
`DeclModifiers`' bits). The rest is genuinely text: names, scopes, type and return spellings, macro values and
aliases, include spellings. Converting `MacroFact::name` or `IncludeFact::spelling` to `SmolStr` removes ~160 000
allocations and **no bytes** (both are 24 bytes either way).

## The read-ahead budget

`Session::READ_AHEAD_BUDGET_BYTES = 32 MB`, enforced by `trim_the_read_ahead`, called on both insert paths:
`commit_a_wave` (the server's pump) and `advance` (the in-process one). The bound used to be a *count*
(`steps * 4` entries), which is not a bound on bytes — the largest prepared summaries are standard-library headers
at a megabyte each.

**Status: implemented, not yet measured end to end.** What is verified:

* both crates build, and the 856 library tests and the `cpp_ls` suite pass;
* `tests/read_ahead_probe.rs` has a test asserting the budget — but its synthetic project's waves were not taken
  (`prefetched` stayed empty), so **that test currently passes vacuously** and must be made to drive
  `prepare_background_wave` + `commit_a_wave` before its number means anything;
* the server-side trace that produced the 183 MB figure (`prepared-not-committed N (X MB)` in the pump's `[memory]`
  line) did not print in the last attempt, because the line is gated on `probes::allocations()` and the probe drove
  the *installed* binary's log without it firing.

So the claim "183 MB → ≤32 MB" is **a bound in the code, not a measurement**. The next session's first job is to
make that line print and record the number.

## Where the memory problem actually is

Not in the struct any more, and not in the cache keys: a context is `(configuration, the file's own directory)`,
measured as 1 761 keys for 1 761 files with 1 macro environment over the whole closure, so nothing is stored 135 times
and the hit rate is not suppressed by environments (see the correction in `open-questions.md`).

The problem is that **nothing bounds the set of files the session keeps**. `ProjectIndex::summaries` and the VFS both
grow with everything ever read — 113 MB and 94 MB on this project — and the query API is why they cannot be bounded
cheaply:

* `ProjectIndex::summary()` answers `Option<&FileSummary>`, and 77 query sites rely on that borrow;
* `Session::text()` and `view()` answer from `vfs.held()`, also `&self`, also borrowed;
* so "evict and reload on miss" needs `Arc<FileSummary>` + interior mutability in both, which is a type-level change
  of the same size as the `DeclDetail` one, not a policy switch.

That is the next change, and it is the only one left that moves the *shape* of the number rather than a constant:

```text
A. summaries: HashMap<String, Arc<FileSummary>> behind a lock — no policy change, everything green
B. a byte budget + LRU + a reload hook the store injects (it owns the redb cache), `cache.redb` on miss
C. the same for the VFS text and line indexes, with open buffers pinned
D. re-measure the editor scenario (40 files × 8 requests) against the 4.8 GB peak
```

The prefetch work above is independent of that and worth having either way: it is the one part of the working set that
is *pure* read-ahead, prepared for a queue that may no longer exist.

## The bottleneck, stated once

The analysis treats **"what a compiler sees for one file"** as the unit of memory and keeps the union of those units
for the session. The design bought zero-lock, zero-copy queries by making the index immutable during a query, and an
immutable index cannot be evicted. Every optimisation so far — the allocator, the inlay path, the fact width, the
`DeclDetail` box — has cut a constant under that ceiling. The working-set change is what lifts it.

## The 5.1 GB seen by a probe, attributed

A probe that indexes this project's whole include closure in **one process** and pumps without a bound was measured
at **5.1 GB** of working set — the same ceiling as the 4.8 GB editor scenario above, reproduced by a different
route. The attribution comes from `Session::memory_report`, printed every ten slices and after the pump:

```text
pumped 36 slice(s)   (the queue drains in 36)   266 MB held
  summaries  119 MB   vfs   83 MB   index tables  40 MB   cooked 7 MB   units 1 MB
```

So the bound is not one bucket: it is the **steady state of the closure** (summaries + the texts read + the tables),
held for every file the session may still be asked about — exactly what the plan A–D above is for. What the number
also says is that the *incremental* cost of a request is small: 266 MB is the whole index of this project's
closure, and the probe's growth after that point is the pump cooking files it will not be asked about again.

The probe is bounded now (`CPPLS_PROBE_SLICES`, default 400) and prints the report, so the next measurement of this
kind costs one slice-length rather than a machine.
