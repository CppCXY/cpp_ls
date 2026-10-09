# References: the cost model, the measurements, and the shape an index must have

Written after two rounds of measuring rather than guessing, and it exists because the first design idea in it — "do
the local variables first, they are most of the uses" — was **wrong by a factor of sixty**, and the number that
showed it took one probe.

Every figure below is measured on this machine: `examples/use_index_cost.rs` for volume,
`examples/body_weight.rs` for the node split, and the source citations are to this repository.

---

## 1. What `references` does today, and what it costs

`ProjectIndex::symbol_references` (`index/references.rs:441-502`) resolves the cursor to a symbol and then walks
**the declaring file plus everything that transitively includes it**:

```rust
for path in symbol_candidates(index, symbol, budget, &mut answer) {
    let Some(text) = files.read(&path) else { … };              // ← a whole file, per candidate
    if !text.contains(&symbol.last_segment) { continue; }        // the cheap filter, and it is already here
    let here = identifiers(&text, &symbol.last_segment);         // a lex, only for files that matched
    …
}
```

So the cost is:

```text
  read        one whole file per candidate        ← the part an index removes
  contains    a substring scan per candidate      ← already there, and it is why the lex is rare
  lex         only for files containing the name
```

**The read is the target.** A file that does not contain the spelling is read in full and then discarded, and in a
header-heavy project most candidates are that file.

The answer's *quality* is a separate matter and a worse one (`docs/review-vs-clang-clion-vs.md` §3.2): the label
`ReferenceKind::Use { resolved_to: symbol.declared_in }` is a **claim** — the list is every identifier token whose
spelling matches, in every file that can see the declaration. Renaming is gated to macros for that reason. An index
that only makes this faster makes it *fast and still wrong*, so the two are one piece of work.

---

## 2. The measurement that killed the first plan

The plan was: local variables first, because 64.7% of identifier uses are inside a statement block and a local
reference is a **single-file fact** — the one kind of fact that satisfies IntelliJ's stub invariant, which
`docs/design-review.md` §3.2 records that our summary layer does not.

```text
  126 files (a standard-library closure)

  identifier uses                            299 143
    …inside a statement block                193 507    64.7%
    …resolved to a LOCAL declaration           2 881     1.0%     ← the number that was missing
  distinct names                              21 328
```

**Being inside a statement block is not being a local.** The overwhelming majority of in-body identifiers are member
names (`push_back`, `size`, `begin`) and namespace members (`std`), which are the cross-file index's business. The
local half of the problem is **1% of the uses**, not two thirds.

The cost side, after one optimisation (see §3):

```text
  local references as a per-file table        31 268 bytes    0.2% of the summaries on disk
  resolving them                              288 ms / 126 files = 2.3 ms per file, 1.0 µs per identifier
```

Cheap in space, affordable in time — and worth 1% of the problem. **So it is not first.** It is a small, real
capability (rename and `documentHighlight` on a local, unused-variable checks) and it is *also* the wrong thing to
persist: at 2.3 ms per file it should be computed **on demand for the file a request names**, which needs no summary
field, **no codec change and no format bump**. That is the round's most useful outcome: a measurement that removed
work rather than adding it.

---

## 3. The optimisation that made the local half affordable at all

The first implementation walked the scope chain for every identifier and **scanned each scope's binding list
linearly**. `Scope::bindings` is a `Vec` on purpose — a name may be declared twice — and the **file scope's** list
holds every top-level declaration in the file, so every one of 299 143 identifiers walked thousands of entries:

```text
  before   one linear scan per scope per identifier    9 222 ms    30.8 µs per identifier
  after    a name map per scope, built once              288 ms     1.0 µs per identifier      32×
```

The lesson is the one this repository keeps re-learning and the reason `docs/indexing-performance.md` §3 exists:
**a linear scan is fine once and quadratic when it is inside the loop.** It was found by reading the number, not the
code — the code looked like a lookup.

---

## 4. What an index has to store, and the two ways to store it

The read is what has to go. Two candidate representations, with the volume each implies:

```text
  (a) per-file Bloom filter over the identifier names the file mentions
        one fixed blob per file, no names, no offsets
        answers "might this file use this name" — so the read, the `contains` and the lex are skipped together
        estimated 512 bytes × 126 files ≈ 64 KB ≈ 0.3% of the summaries
        a false positive costs exactly what today's code costs for that file and nothing more

  (b) per-use offsets, name-table indexed
        exactly what §6 of the other document proposed, and what the projection measured:
        2 478 456 bytes ≈ 13.4% of the summaries
        answers the query without reading anything at all — but only for the files it was built for
```

**(a) is the first step and (b) may never be needed.** The reason is not the 13.4% — that is affordable — it is that
(b) makes every summary decode pay for a table that completion, hover and diagnostics never read, while (a) is one
blob that a single `might_contain` reads.

`FileSummary`'s own cost is not small: **147 KB per file** on average over this closure (18.5 MB / 126). A use table
at 13.4% is 20 KB per file added to a structure that is loaded for every file a query touches.

---

## 5. The shape, and the two things that must not change later

**Keyed by name, never by resolved symbol identity.** This engine has no overload resolution and no template
deduction, and its contract is three-valued. An index keyed by resolved identity would have to write `Unknown` as
*not a reference*, which is the one thing this crate refuses to do anywhere. A name-keyed **candidate set** is true
today and gains a resolution column later:

```text
  today      name → candidates → verify each (resolve the use, compare identity)
  overloads  the verification gets stronger; the key does not move
  deduction  a use whose target is a dependent name stays a candidate with state "not yet verified" —
             it must be **present and unverified**, never omitted, or there is nothing to fill in later
```

So the record is `(name, offset, kind-of-use)` with a state, and `UnknownReason` gains a variant for *"dependent,
not yet deduced"* rather than folding it into an existing reason. That vocabulary is closed on purpose — every
`match` fails to compile when a variant is added, which is what keeps the reasons specific.

**And the second implementation must die in the same commit.** `docs/review-vs-clang-clion-vs.md` §2.3 records three
defects that are all one question asked twice. The identifier scan in `symbol_references` and this index are the same
question; keeping both is the fourth.

---

## 6. Order, with the acceptance for each

### 6.0 What was built, and the measurement that leaves it unproven

The filter is in: `FileSummary::use_filter`, `summary::use_filter_of` (built from the **tree**, so a name in a comment
or a string is not a use), `summary::use_filter_might_contain`, coded after the module reading, `CODEC_VERSION`
27 → 28, and `symbol_references` consults it before reading. It costs **74 808 bytes on a 126-file closure — 0.40%**
of the summaries, against the 13.4% that per-use offsets would take.

**And on the workload measured, it rejects nothing.**

```text
cargo run --release --example symbol_references_cost -- <dir> <entry.cpp>

  size        candidates 3   filter rejected 0   read 3 files/107 KB in 213 µs   query 8.5 ms
  begin       candidates 3   filter rejected 0   read 1 file/77 KB  in 137 µs   query 0.63 ms
  push_back   candidates 4   filter rejected 0   read 1 file        in  92 µs   query 8.2 ms
  value_type  candidates 2   filter rejected 0   read 1 file        in  97 µs   query 7.6 ms
```

The reason is the shape of the fixture and not the filter: the candidates are *the declaring file and everything that
transitively includes it*, and a **one-translation-unit closure has two to four of those**. The read the filter
removes is 92–213 µs, so the whole field is worth about a tenth of a millisecond per query here.

**Where it would pay is the shape `find_references` measures on the macro path** — the same closure with an entry
file that pulls in the Windows SDK gave `__attribute__` **171 candidates, 145 of them containing no occurrence of the
name**. A project of many translation units including one header is that case, and a symbol declared in such a header
is the case this field exists for. That case has **not** been reproduced here, and the field is therefore kept on the
argument that a false positive costs one read while the storage is 0.40% — not on a measurement. That is recorded
rather than dressed up, because the alternative is a field whose justification is a number somebody remembers.

### 6.1 The order from here

```text
1.  Measure the symbol path on a multi-translation-unit project — `symbol_references_cost` against a directory with
    several `.cpp` files including one header. If the candidate sets stay small there too, **delete the filter**:
    0.40% of every summary decode for a tenth of a millisecond is not a trade, it is a habit.

2.  Resolve rather than spell-match. `local_references_of` (`sema/declarations.rs`, built and measured this round:
    2.3 ms per file, 1.0 µs per identifier after a 32× optimisation) answers the local case; `definition` answers the
    rest. A candidate that resolves to a *different* declaration is dropped instead of labelled as a use.

3.  Delete the identifier scan, in the same commit as (2).

4.  `codeLens`, lazily, on `codeLens/resolve`.

**Not on this list: persisting per-use offsets.** §4 sizes it at 13.4% and this round's measurement says the query it
would accelerate costs **8.5 ms**.
```

