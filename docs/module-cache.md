# The module cache: an environment is a value, and a parse is a function of it

Status: design, not implemented. Everything marked **measured** was measured on `EmmyLuaCodeStyle` or `test_cpp`; the
rest is a claim with the test that would settle it.

This is the change `docs/memory-status.md` calls "the only one left that moves the *shape* of the number". That page
establishes that the retained bytes (summaries 113 MB) are no longer the problem; this one is about the two numbers
that are: **what one query pays for** (2.8–8.8 s and +174–355 MB per whole-program read) and **what the session keeps**
(the union of every closure it ever walked).

## 1. The claim

A file's reading is **not** a function of its translation unit. It is a function of three things:

```text
text of the file          its own bytes
context                   configuration + the file's own directory (quoted includes)
environment               the macros in force at the point it is included
```

The translation unit is how a compiler *arrives* at the environment; it is not the environment. Today this analysis
confuses the two: to read one file it walks the whole closure (`TranslationUnit::walk`, one text scan per file in the
closure — **measured**: `LuaCheck.cpp` reaches 373 files, 844 940 tokens, 8.8 s), and it keys the result by the
closure (`SummaryStore::the_closure_fingerprint`: a hash of every file's path and content in the closure).

The change is to make the middle value first-class:

```text
Env    a persistent map  name -> definition,  with a Merkle hash (EnvId)
Site   (file content, context, incoming EnvId)          -- an inclusion point, memoised
Parse  (file content, context, reachable EnvId) -> facts -- the cache entry
```

Then reading a file costs **the include spine**, not the closure: a fold over per-file *directive skeletons* (already
in `FileSummary`), each memoised by `Site`. Nothing re-scans a file's text to answer a question about another file.

## 2. Multiplicity: the same file, many environments

The question "can one file have several environments?" has three separate answers, and the design has to get all
three right.

### 2.1 Across translation units — yes, and that is C++

`a.cpp` and `b.cpp` can include `api.h` with different macros in force (`-D`, a `#define` before the include, a
`#if` around it). Then `api.h` has two readings, and a cache keyed by the file alone would answer one for the other.

**Measured** (`docs/open-questions.md`, the "135 contexts" correction): on `EmmyLuaCodeStyle`'s closure, 1 761 files,
1 723 distinct content hashes, 38 files stored twice, and **1 distinct macro environment over the whole closure**. A
kernel- or Chromium-shaped project has several environments (one per configuration variant); the number of *entries*
is `distinct (content, reachable env)` pairs, not `files × roots`.

### 2.2 Within one translation unit, at several inclusion points — yes, and that is what guards are

`bits/version.h` in libstdc++ is deliberately **unguarded**: every header includes it to add the feature-test macros
*that header* asks for. So the same file is entered several times in one program, and its environment differs each
time. `TranslationUnit::walk` already knows this (`walk_one_file`'s doc explains why a second read is not skipped for
an unguarded file). The key must therefore be the **site**, not the file:

```text
SiteId = (content_hash, context_hash, incoming EnvId)
```

The incoming `EnvId` captures exactly the state at the `#include`, so two sites with the same state share one entry,
and `from_offset` is not in the key (it is in the diagnostic data, not in the answer).

### 2.3 Within one translation unit, differing in macros the file never mentions — no, and that is the sharing

An environment is a *reachable* value from the file's point of view. If 1 000 macros differ between two programs and
the file mentions none of them, its reading is the same, and the key must say so or the cache degenerates into
"every header once per program". This is what `MacroView::environment_fingerprint` already computes (see §4).

**So the answer to "does the same file get several environments?" is: only as many as the distinct *reachable*
environments that reach it.** The raw reading (§5) is shared across all of them; only the rendered form is per
environment.

## 3. The model

### 3.1 Env: a persistent map with a hash

```rust
struct Env {                      // persistent, structurally shared
    parent: Option<EnvRef>,       // O(1) snapshot of an earlier state
    overrides: SmallMap<NameId, Option<MacroId>>,  // this step's #define/#undef
    id: EnvId,                    // Merkle hash of (parent.id, sorted overrides)
}
type EnvId = u64;
```

`define`/`undef` copy a path of nodes (a handful) and update the hash on the way out: O(log₃₂ N) with O(1) hash work,
no table copy. Two states are equal iff their ids are, so **key comparison never walks a table**.

`MacroId` is an interned definition: `(NameId, function_like, parameter_text_hash, body_text_hash, body_tokens_hash)`.
The body hash must cover what expansion reads — the replacement list as tokens, plus the parameter list — and nothing
else (whitespace and the definition site are diagnostics, not meaning).

### 3.2 Site: the memo that replaces the walk

```text
compose(content, context, incoming: EnvId) -> SiteResult {
    env_at: Vec<(offset, EnvId)>,   // the state after each #include, for diagnostics and for nested includes
    final:  EnvId,                  // the state at the end of the file
}
```

`compose` is a **fold over the file's directive skeleton**, not over its text:

```text
for each directive in order (from FileSummary.macros / .includes, ordered by range):
    #define/#undef  -> env = env.define(name, id) / env.undef(name)
    #if/#elif/#else -> evaluate the stored region question against env
                       (the branch is already flattened: a fact's guard names its region)
    #include        -> site = SiteId(child_content, child_context, env.id)
                       child  = compose(child_content, child_context, env.id)   // memoised
                       env    = child.final
```

Everything it reads is already stored per file: `MacroFact` (name, kind, function like, body, `value`, `alias`,
`range`, `guard`), `IncludeFact` (spelling, resolved, `is_next`, `guard`), and `SummaryGuards` (the regions and their
questions — "a region is stored as its **question**, never as its answer", which is exactly what an environment is
for).

**Cost:** one memo hit = one hash lookup, then the answer (`final EnvId`) is in hand. A miss = the file's directives
(typically 20–400) × (a name interning lookup + a condition evaluation over a few tokens) + one memo lookup per
include. There is no lexing of the file and no parse of the file.

### 3.3 Keys

```text
RawKey     (content_hash, context_hash)                          -- the file's own syntactic reading
ParseKey   (content_hash, context_hash, reachable EnvId)         -- the rendered reading, when one is asked for
SiteKey    (content_hash, context_hash, incoming EnvId)          -- the compose memo
```

`RawKey` is today's `SummaryKey` **without `env_hash`**. That is deliberate and it is the largest single saving: the
raw reading stores every fact *with the region it was written in* (`FactGuard::Region`), so it does not depend on the
environment at all — a guarded `#define` is recorded in both branches, each tagged. One entry per file, shared by
every program that reads it. **Measured**: `EmmyLuaCodeStyle`'s summaries are 1681 entries / 43.4 MB, and the cooked
readings are 388 / 13.2 MB — today both are already close to per-file, but the *closure key* on the cooked side
(section 4.3) is what must change.

## 4. The reachable environment: what is hashed, and how fast

### 4.1 What must be in it

The mention set of a file is a pure function of its text, and is computed once per content hash:

* every **identifier token** of the file (it may be a macro call);
* every name in a **condition** (`#if A && B`, `#ifdef C`, `defined(D)`), including inside `#elif`;
* the **argument of an `#include`** when it is a macro (`#include HEADER`);
* every name **mentioned by the body of a macro** in the set, transitively — expanding `A` looks up `B`;
* **not** the seed (`-D`, the compiler's predefines): that is `context_hash`'s half (see
  `environment_fingerprint`'s doc, which states this).

### 4.2 The fast path

```text
reachable(content, incoming: EnvId) -> EnvId
    mentions = mentions_of(content)                 // memoised by content hash: Vec<NameId>, sorted, deduped
    pairs    = mentions.iter().map(|n| (n, env.lookup(n)))   // O(k) hash lookups, u64 ids only
    hash     = fnv1a64(pairs as little-endian u64s)          // no strings, no allocation
```

with a second memo `(incoming EnvId, mentions_hash) -> reachable EnvId` for the hot path (a hover on the same file
while the environment is unchanged). Everything is `u64`:

| step | today | this design |
|---|---|---|
| whole-table hash (`environment_hash`) | O(M log M) with a `String` per name and per body; M = every macro in force (10⁴–5·10⁴) — measured at **~1–3 ms** and ~1 MB of transient per key | not on any query path |
| reachable hash | `environment_fingerprint` exists but is not the key: O(k) with `&str` keys and a sort, k = mentions (10²–5·10³) | **~5–40 µs**, u64 only |
| memo hit | — | **~50 ns** (one hash lookup) |
| compose memo hit | — | **~50 ns** per include edge on the spine |

The point of the interning is that **no key is ever built from text**. A `MacroId` is computed once, when the
definition is first indexed (the file is being read anyway); from then on a definition is seven numbers.

### 4.3 Where today's cost actually is

Three different "environments" exist in the code, and only one of them is a key:

1. `SummaryStore::environment_hash(facts)` (store.rs) — the whole macro table, sorted, as bytes. Used when a summary
   is written from a rendering. **Measured** as the ~1–3 ms above; it is also what destroys sharing when two programs
   differ in a macro nobody's file mentions.
2. `SummaryStore::the_closure_fingerprint(path)` (store.rs) — path + content over the whole closure, sorted FNV.
   This is the cook-record key, and it is why a per-file cook record is still per program: two programs with the same
   header get two records. Replacing it with `reachable EnvId` is what makes the record shared. (**Measured** on
   `test_cpp`: 106 stored cooks served in one warm run; on a project with any environment variation the same header
   would be re-cooked once per variant.)
3. `MacroView::environment_fingerprint(source, tokens)` (summary.rs) — the right function, wired to nothing that is a
   key yet.

## 5. Sharing, layer by layer

```text
raw reading       one per (content, context)                1681 entries, 43.4 MB measured
rendered reading  one per (content, context, reachable env) 388 entries, 13.2 MB measured
parse tree (view) one per rendered reading, LRU              bounded (VIEW_BYTES)
```

A header included by 40 programs under 40 environments costs **one** raw reading and 40 rendered readings *only if
someone asks for a rendered form in all 40*. Queries that work from facts (definition, references, hover of a type,
completion of a qualified name) ask for the raw reading and the environment at the site — no render at all.

## 6. Soundness: what the key must include, and why

| the reading depends on | where it is in the key | the test that would catch a mistake |
|---|---|---|
| the file's bytes | `content_hash` | `a_stored_cook_answers_the_next_session` (exists) |
| the directory (quoted `#include`) and the configuration (`-I`, `-D`, `-std`) | `context_hash` | `resolution_still_holds` (exists, re-resolves every include on a hit) |
| the state at the include point | `reachable EnvId` / `incoming EnvId` | a differential test against `TranslationUnit::walk` on a corpus (§7, gate 1) |
| `__LINE__`, `__FILE__` | the site: if the file mentions them, add `from_offset` (and the includer's path) to the key | a fixture that includes one header from two offsets and reads a `__LINE__` value |
| `__COUNTER__` | not cacheable per site; mark the file, recompute | a fixture with two includes of a `__COUNTER__` user |

**The differential test is the gate.** `compose` must produce, on a corpus of real headers, the same *answers* as
`TranslationUnit::walk`: for every file in a unit, and a fixed list of probes (is `X` defined, what is its body, is
this region in force), the composed environment must agree with the walked one. That is a stronger and cheaper test
than checking that the two produce the same `EnvId`.

Two known deviations that must stay deviations, and are documented where they are implemented:

* an include that does **not resolve** (`has_unresolved_includes`) is not a candidate for the raw cache — a summary
  that records a failed search must not be stored, because nothing in the key would notice the header appearing;
* a summary built **from a rendering** is not stored under the raw key, for the reason store.rs already gives.

## 6a. What exists (end of the first implementation round)

`crates/cpp_code_analysis/src/cache/env/` — four files, and the test module inside the fourth:

```text
mod.rs        Env (a persistent chain with a Merkle id), Definition, EnvId, and `Over` — the table a
              condition sees, the environment in front of the compilation's own definitions
skeleton.rs   a file's directives in source order, bodies sliced from `body_range` and trimmed
compose.rs    the fold, with the include recursion behind an `IncludeHandler` so the unit-wide
              shape reuses it (`compose` and `compose_unit` are the two handlers)
unit.rs       every file's entry and exit state from one traversal
tests.rs      six tests: define reaches its includer, undef takes it away, `#if 0` is not composed,
              a guard's second inclusion changes nothing, one file one id, a function-like body
              starts after its parameters
```

Two things the implementation found that the design note had not:

* **A guarded file contributes nothing when its own guard is not taken.** Its facts are stored
  `Unconditional` (`deguard_the_files_own_guard`), so without `SummaryGuards::own_guard_holds` the second inclusion of
  a guarded header applied its `#define` again and the two states hashed differently. The fixture measured it.
* **`body_range` starts at the byte after the name**, so a replacement list slices with a leading space; trimmed,
  because two spellings of one definition must be one environment.

**Gate 0, first real run (EmmyLuaCodeStyle, one root, 2026-11-18).** The fixture gate is green: 3 files, every name
compared, 0 unknown guards, 0 mismatches, on every test run. The probe is not:

```text
CodeFormat.cpp: 1025 files walked, 907 727 names compared, 209 626 mismatches, 2 093 unknown guards
                only walk 196 960 | only fold 8 773 | body 3 862 | kind 0 | missing files 31
```

The examples are all of one shape — `__cpp_lib_*`, `__glibcxx_want_*`, `_GLIBCXX_*_H` — feature-test macros defined
inside `#if` regions whose condition needs a **value** (`__cplusplus > 201703L`, `_GLIBCXX_USE_CXX11_ABI`,
`__has_include(...)`). The fold answers those `Unknown`, applies nothing, and loses the whole branch's names: 2 093
undecided regions are enough to account for ~197 000 names if each gates ~90 definitions, which is what libstdc++'s
feature-test headers do. So the dominant term is the known gap (§8: a `Definition` answers `DefinedWithoutAValue`),
not a disagreement about a decidable rule. `missing files 31` and `only fold 8 773` are the second-order terms and
need their own attribution (per-file unknowns, and the fold's own-name exclusion).

**Second run, after the value answer.** `Definition` now carries the one value a summary can prove — `#define A 1`
gives a `MacroDef` whose `value` cache answers `#if A` without a body — so the libstdc++-configuration class of
undecided regions fell out:

```text
                        before      after
mismatches              209 626     111 366
only walk               196 960      98 591
unknown guards            2 093       1 914
names compared          907 727     913 261
```

What is left is concentrated in four headers — `bits/version.h` (51 undecided), `cstdlib` (46), `_mingw.h` (28),
`cppconfig.h` (10 + the `_GLIBCXX_HAVE_*` family) — and the conditions are `__has_builtin`, `__has_include` and
friends, where the *table* has to answer and a bare environment cannot. The walk and the fold are confirmed to share
the policy (`a_guard_holds` is `visibility == Active`, so both skip an undecided region); the remaining 98 591
`only walk` names are therefore **not** explained by the 1 914 undecided directives alone — 1 914 skipped definitions
cannot account for 98 591 names — and the next round has to attribute them per file before anything is concluded.
`only fold 8 450` is the mirror image (names the fold has and the walk does not) and needs the same attribution.

**Third run: the disagreement is include resolution, not guard evaluation.** The probe now reports what the traversal
reached:

```text
composition: 251 file(s), 778 unresolved includes      the walk: 1053 file(s)
```

The fold follows `IncludeFact.resolved` — the path the resolver found when the summary was built — and on a project
with no compile database **778 of the closure's includes did not resolve at index time**. The walk does not use that
field: `closure_with_text_from` builds its own closure with the session's resolver over every file the index knows, so
it reaches 1053 files. That is the residual: 800 files the fold never sees, and the 61 028 `only walk` names they
carry — which is also why the per-file list shows the *same* set of names missing from every file rather than a
different set per file.

So the next change to `cache::env` is structural: **composition takes a resolver**, the way the walk does, instead of
trusting a summary's cached resolution. `IncludeFact.resolved` stays useful as the *cached candidate* — it is what
`resolution_still_holds` re-verifies on a summary hit — but the environment must not make it the only path.

The alias answer also needs one guard rail before it is trusted: `_GLIBCXX_HOSTED` is an alias to a **predefine**, and a
chain that ends at a name neither the environment nor the seed holds must answer `Unanswered`, not `0`, when the seed
is `Marked::incomplete`. The 1032 → 251 file count between the two runs is that distinction showing up in the
traversal, and it has to be resolved before anything is concluded from either number.

**Fourth run: the 778 are headers that do not exist, not a resolver gap.** With the resolver instrumented: 6 222 includes asked, 5 444 cached, **0 rescued** by suffix, 778 unresolved --- and the missed spellings are `machine/ansi.h` and `sys/_types.h`, which are in `bits/c++config.h`'s `#if __has_include(...)` blocks: BSD headers this mingw snapshot does not have. The walk cannot follow them either, so the 778 are not why the fold reaches 251 of the walk's 1 053 files.

The remaining question is instrumentable: **for every include step the fold skips, which `Visibility` did it get?** An `Inactive` hides a whole subtree without adding to `unknown_guards`, and 527 unknown directives cannot hide 800 files. **Seventh run: the definitions expand, and the residual is one small class.** Definition now builds the MacroDef a condition can *expand* — parameters sliced from between the name and the body, body tokens re-lexed from the body text — so a function-like alias (#define _GLIBCXX_HAS_BUILTIN(B) __has_builtin(B)) is evaluable. The numbers collapsed:

``text
                    before      after
mismatches          74 425     35 430
only walk           61 028     34 949
only fold            8 353        481
body differs         5 041          0
unknown guards         527        188
missing files            3          0
walk-only files         3          0
`` 

ody 0 and only fold 481 say the two implementations now agree on **what a definition says**; what is left is 188 undecided directives (__has_include, __has_feature, __cpp_if_consteval) whose skipped #defines are invisible to ~350 files each — 35 000 names, the same multiplication as before. **Eighth run: the remaining 188 classified.** The tally had been reading the *innermost* region; isibility_of answers Unknown when **any** enclosing region is undecided and guard_text lists them outermost first, so the class is the last segment. With that corrected, the 188 are: 16 #ifndef-shaped regions, and the rest !defined(__cpp_lib_bit_cast), !defined(__cpp_lib_is_layout_compatible), !defined(__cpp_lib_source_location) and siblings — that is, defined(...) on names the **index marked uncertain** (Marked::mark_uncertain), which Marked::lookup answers Unanswered and the evaluator turns into Unknown. __cplusplus is *not* among them: Over answers Defined with 202002, verified directly. So the remaining class is the index's own uncertainty list, and the next question is whether the walk is uncertain about the same names — if it is, those entries are 'both unknown' and must be excluded rather than counted, and the 34 949 only-walk names come from somewhere else.

**Ninth run: the missing names have no definer in the composition.** Searching every composed file for each of the first missing names answers Definer None for all of them (__glibcxx_want_byteswap, __glibcxx_is_aggregate, …). So the fold is not failing to *propagate* them: it never applies the definitions at all, inside its/version.h, because their regions are the 188 undecided ones. The arithmetic is now closed: a skipped definition inside a shared header is invisible to every file that includes it, and the per-file count (151) × the number of consumers (~350) is the 34 949.

What remains is one mechanism question, and it is about **the index's uncertainty list**: !defined(__cpp_lib_bit_cast) is answered Unanswered because Marked::is_unknown says so, and the walk's UnitMacros asks its own state first. The next instrument answers, for one name: is_defined, is_unknown, and whether the walk recorded the definition that would settle it.

**Tenth run: the chain is one `#define` the fold never applies.** Per-name answers, the most specific data so far:

```text
__cpp_lib_bit_cast:        defined false, unknown false, in walk false, in fold false
__cpp_lib_source_location: defined false, unknown false, in walk false, in fold false
__glibcxx_want_bit_cast:   defined false, unknown false, in walk true,  in fold false
```

The uncertain-name hypothesis is dead: none of the three is uncertain, and `!defined(__cpp_lib_bit_cast)` is
*decidable* (`Undefined`), so the undecided segment is elsewhere in the chain. What is concrete is the third line: the
walk's views contain `__glibcxx_want_bit_cast`, the fold never defines it anywhere. That name is written by the
**includer** (`<bit>` writes `#define __glibcxx_want_bit_cast` before including `<bits/version.h>`), so the fold is
skipping an unconditional `#define` in a header it composed. The next instrument is the smallest one left: is `<bit>`
among the composed files, and if it is, what are its `unknown_guards` and the steps named `__glibcxx_want_*`.

**Eleventh run: the feature-test pattern, read off the headers.** `bit:58` writes five `#define __glibcxx_want_*`
and includes `<bits/version.h>`; version.h's block is

```cpp
#if !defined(__cpp_lib_bit_cast) && defined(__glibcxx_want_bit_cast)
#define __cpp_lib_bit_cast 202102L
#endif
#undef __glibcxx_want_bit_cast          // always, outside the #if
```

so the `want` macro is *consumed and undefined* by the header that wrote it, and `__cpp_lib_bit_cast` is what the
consumer keeps. The probe confirms `<bit>` is composed with **0 unknown guards**, and `<bits/version.h>` is the
deliberately **unguarded** file `walk_one_file`'s note describes — the one a compiler re-enters for every header.

Both names are absent from every composed file's `at_exit`, which the `#undef` explains for the `want` name and not
for `__cpp_lib_bit_cast`. So the next instrument is `<bit>`'s own skeleton: the steps whose names are
`__glibcxx_want_*` and `__cpp_lib_bit_cast`, with the visibility each got. That distinguishes a guard decided false
(the top of `bit` is inside `#if __cplusplus > ...`-shaped text) from a step that never made it into the skeleton.

The class is named and small; __has_include needs an include search in the table, and the other two are compiler predefines this machine's seed does not carry.

**Earlier note.** **Sixth run: the closure is complete.** The set difference says it outright: `walk-only files: 3` --- `bits/ranges_algobase.h`, `bits/std_mutex.h`, `numbers`. The walk's 1 053 is a count of **frames**, not files (`unit.files()` yields one entry per frame, and a header included several times has several), and the fold's 251 distinct files are the walk's distinct files minus three. So the traversal was never the problem; the previous rounds' `251 vs 1 053` was a comparison of two different units.

The name gap is therefore **inside the files both sides reach**: for a file, the fold's `at_entry` lacks names the walk's view has, while the file itself is composed. The next instrument attributes a few missing names to the file that defines them (from the composition's own files, by scanning their `at_exit`) and prints the guard the fold decided for that definition --- `Inactive` there would mean a condition decided the wrong way, `Active` would mean a definition that never entered the environment at all.

**Earlier note, kept for the record.** **Fifth run: no look-up fails either.** The look-up is instrumented on both branches (summary(target) and 	ext(target)) and across the whole traversal it reports **0 missing summary, 0 missing text**: every include the fold takes is looked up successfully. The fold's closure is 251 files *by its own include edges* --- 6 222 edges asked, 5 444 with a cached resolution, and they close over 251 files. So the walk's 1 053 files are not lost at a look-up and not skipped by a guard; they are **not in the summaries' include edges at all**, and the next instrument is the set difference: the walk's frames minus the composition's files, printed with their first ten paths. That list names the shape (one header family, one directory, one resolution rule) instead of asking the reader to guess it.

**Earlier note, kept for the record.** The skipped-include tally came back empty: **no include is skipped** — every include step the fold reaches is Active, and the 527 unknowns are #defines, not includes. So the traversal is not losing files to visibility at all; it stops because look_up returns None for an include the fold *took*. The two candidates there are session.index().summary(target) and session.text(target), and the next round counts which one fails, with examples. The walk does not depend on either the same way: its closure_with_text_from pre-reads every text in the closure, so a file the fold loses at 	ext is one the walk still walks. build the `MacroDef` a `Lookup::Defined` needs from a
`MacroFact` (`value` is the single integer literal the summary already stores; `body_range` gives the rest) and answer
it from `Over`. Then re-run this probe: `unknown guards` should fall, and the mismatches with it. Only when the only
remaining mismatches are attributed and understood is S1 (deleting the walk from the cook) safe.

**Earlier note, kept for the record:** Gate 0 was not yet proven. The harness needs the *walk's* answer per file, and the two obvious routes do not give it:
`Session::read_the_unit` may be served from a stored rendering (no timeline, `TranslationUnit::units` empty), and
`gather_a_unit_read` answers from the cache for the same reason. The next step is a test-only accessor that forces a
cold walk — `closure_with_text_from` + `TranslationUnit::walk`, the pair `read_the_program` itself uses — and compares
`environment_of(file)` against `compose_unit`'s `at_entry` on the names the file's own subtree does not define. The
comparison rule, the exclusions and the counters are written in the round's notes and in §8.

A gap the fixture already caught, and the reason Gate 0 matters: `#if API_VERSION > 2` is **undecidable today**, because
a `Definition` answers `Lookup::DefinedWithoutAValue` — the summary stores a body as a shape plus one literal, not the
token list `macro_value` reads. With `Over` in front of the compiler's own table the seed's values are decided, and the
file-defined ones are the piece `docs/module-cache.md` §8 lists. A fold that meets an undecidable guard applies
nothing and counts it, so Gate 0 must report `unknown_guards` beside its mismatches rather than pretending they are
zero.
## 7. Staged plan, with the number that decides each stage

**Gate 0 (a day).** A differential probe: for 20 files of `EmmyLuaCodeStyle`, `compose` vs `walk`, with 50 probes per
file. It runs before any production change; if it disagrees, the design is wrong and nothing else matters.

**S1 — delete the walk from the cook (2–3 days).** `cooking_materials` keeps only the composed arm; `compose` above;
`the_closure_fingerprint` replaced by `reachable EnvId` for the cook record. Gates: Gate 0 green; the C++ corpus stays
51/52; the two directive-split tests pass; single cook on `LuaCheck.cpp` from ~8.8 s to **< 100 ms**; 82 open files in
one `advance(64)` from **2.87 GB to < 500 MB**.

**S2 — the raw cache without `env_hash` (2 days).** `SummaryKey::env_hash` becomes 0 for raw readings; the reachable
env moves to the cooked key only. Gates: on Emmy, the same header read by two `.cpp` files with different macros
serves **one raw entry**; warm start reads 1046 → **0** files (see `memory-status.md` §3).

**S3 — bounded working set (3–4 days).** `Arc<FileSummary>` + LRU + reload from redb; `Arc<str>` for the VFS with open
buffers pinned; views under `VIEW_BYTES`. Gate: Emmy steady-state tables ≤ **256 MB**, private peak ≤ **400 MB**, and
a 10× larger synthetic project leaves both unchanged.

**S4 — the project index as the product (1 week).** The batch path (`inspect`, the census probes) writes the raw
readings and the interning tables; the server loads the name/file tables only and composes on demand. Gate: open a
file in a 100 000-file synthetic tree at the same latency as in Emmy.

## 8. Open questions

* **Condition evaluation in `compose`.** `SummaryGuards` stores the condition *spans*; evaluating them needs those
  few bytes of text. Either the skeleton stores the condition text inline (a few hundred bytes per file) or `compose`
  reads the region from the VFS. The first is bounded and testable; the second keeps summaries smaller. Decide with a
  measurement of the two cache sizes on Emmy.
* **`#include` inside a skipped branch.** The fold must evaluate the guard *before* recursing; a naive fold that
  recurses first would compose environments for headers a compiler never reads — wrong answers on `#if 0` blocks.
* **`#include_next` and the search order.** `IncludeFact::resolved` is re-verified today; a composed environment must
  not become a second, unchecked resolution. Keep the re-verification on the hit path.
* **Transitive include closure vs. direct.** `env_at` after an include is the child's *final* environment, which is
  the child's whole subtree. That is C++ semantics (an include splices its whole text), and it is why the closure is
  a real dependency — the point is that it is a *value* computed by folding memoised deltas, not a *scan*.
* **Where the memo lives.** Per session is simplest; a process-wide (static) memo would share across workspaces but
  must be bounded and cleared by generation. Start per session, measure the hit rate, then decide.

## 9. What this does not fix

Template instantiation, overload resolution and ODR checks stay translation-unit-wide. No C++ front end makes those
incremental, and clangd does not either. They belong to the batch path (S4) and to lazy instantiation on query — a
separate design, out of scope here.
