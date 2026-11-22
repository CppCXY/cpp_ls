# Worklog — what was measured, what was wrong, what fixed it

A record rather than a map. [`c-support.md`](c-support.md) says what C is read as and which rules can fire;
[`cpp-support.md`](cpp-support.md) does the same for C++; this file says what a real program actually reported, which
of those reports were the analysis's mistakes, and what closed them. New entries go at the top.

Every number here was taken with a project's **own** build configuration (a compilation database when the project has
one, a synthesized one from its Makefile when it does not), and every entry names the commit that made the difference.
The protocol for asking one question at a time is [`inspect.md`](inspect.md).

---

## The json `syntax-error`s, reduced to one line: json's own DEFINE body against an equivalent local one

> **Superseded.** This section records the reduction that led to the fix; the 60 `syntax-error`s are **gone** (0 in
> the section "The goal's two json defects, closed", commit `25c1ece4`). Kept because the comparison below is how the
> expander defect was found.

Re-measured in the current tree: **126 of 126 files answered, 190 findings** — 86 `redefinition`, 60 `syntax-error`,
28 `use-override`, and 16 others. The 60 are all one construct, and the second measurement this round narrowed it to a
**single line of difference**, in the same file, at the same position:

```cpp
// json's real macro, from `detail/macro_scope.hpp:687`
#define NLOHMANN_DEFINE_TYPE_INTRUSIVE(...) \
    NLOHMANN_JSON_EXPAND(NLOHMANN_JSON_TYPE_BODY(NLOHMANN_JSON_DEFINE_TYPE_INTRUSIVE_, __VA_ARGS__)(__VA_ARGS__))

// and an equivalent local one, written in the probe
#define DEFINE_LOCAL(T) void to_json3(T& j) { NLOHMANN_JSON_EXPAND(NLOHMANN_JSON_PASTE(NLOHMANN_JSON_TO, a)) }
```

The local one expands **completely** (`nlohmann_json_j["a"]=nlohmann_json_t.a;`); json's own leaves its nested
`NLOHMANN_JSON_EXPAND` calls in place:

```text
friend void to_json(…){NLOHMANN_JSON_EXPAND(NLOHMANN_JSON_EXPAND(nlohmann_json_j["zz_a"]=nlohmann_json_t.zz_a;))}
```

The distinguishing shape is the **call whose callee name comes out of an expansion** —
`NLOHMANN_JSON_TYPE_BODY(…)(__VA_ARGS__)` — reaching `NLOHMANN_JSON_PASTE2`'s body, which is the double
`EXPAND(EXPAND(…))` the leftover shows. `CookedStream.diagnostics` records **no** expansion note for it (the notes are
now dumped beside the rendering, `<file>.expansion`, under `CPPLS_DUMP_RENDERING_DIR`), so the rescan is not *declining*
to expand it — it either never reaches those tokens or ends before them. And the same macro written in the file at the
same position **does** expand (`int NLOHMANN_JSON_EXPAND(zz_marker) = 7;` → `int zz_marker = 7;`), so the table is not
the problem: it is the rescan of a **nested body expansion that itself contains a computed call**.

That is where the next round starts: a `cook_with` fixture with a two-level computed call and a double `EXPAND` in the
callee's body is the smallest thing to try, and `target/bench/paste_probe2.cpp` (json's real 227-line PASTE block plus
a local `DEFINE`) is the fixture that already expands correctly — the difference between it and json's body is the
`(…)(__VA_ARGS__)` step.

## nlohmann/json 3.12.0 — the first **C++** library measured

A header-only library, so the measurement is not one project but three questions: the library's own code, its test
suite, and the shape of the framework the tests are written with. The tree is `E:\corpora\json` (3.12.0):
`include/nlohmann/**` (84 modular headers), `single_include/nlohmann/json.hpp` (the 25 000-line amalgamation), and
`tests/src/unit-*.cpp` (126 translation units using doctest).

The database is synthesized, because a header-only library has no build of its own:

```text
g++ -std=c++17 -I include -I tests/src -I tests/thirdparty/doctest -c tests/src/unit-<name>.cpp
```

| Question | Answer | Findings |
| --- | --- | --- |
| the library, through the **amalgamation** (`probe_amalgamation.cpp`: `#include <nlohmann/json.hpp>`, `-I single_include`, an object literal, `dump()`, `get<int>()`) | **Cooked, clean** | **0** |
| the **test suite**, one `cpp_inspect` process per TU (121 of 126 answered) | 19 of 121 TUs do not parse | **92 `syntax-error`** |
| 5 TUs the tool could not answer | "has no reading to diagnose" | — |

So the library reads clean; what does not is a **macro shape inside the test framework's use**: every failing TU's
first error is reported at its first `TEST_CASE("…")` line (`unit-conversions1.cpp:53`, `unit-binary_formats.cpp:27`,
`unit-bjdata.cpp:4480`, …), and the errors after it fall inside that case's body. A probe carrying the file's own
preamble (16 lines of includes and defines) plus `TEST_CASE("x") { CHECK(1 == 1); }` is **clean**, and so are nested
braced initialisers (`std::map<std::string, std::vector<int>> m = {{"a", {1, 2}}};`) and the exact
`json::object_t o = {{"object", json::object()}, {"array", {1, 2, 3, 4}}};`. The trigger is therefore inside the
bodies the tests write — **open**, with the file list and the repro command in `target/bench/json-sweep.json`. The
bisect so far, each probe a single `cpp_inspect diagnostics --cook` run on a TU that includes only what is named:

| probe | content | result |
| --- | --- | --- |
| the file's own preamble (lines 16-53) + `TEST_CASE("x") { CHECK(1 == 1); }` | | **fails** |
| … with the two `DOCTEST_CLANG_SUPPRESS_WARNING_*` lines removed | | **fails** |
| … with the `#if __has_include(<optional>)` block removed | | **fails** |
| … with `#define JSON_TESTS_PRIVATE` removed | | **fails** |
| doctest + `json.hpp` + `using` + all seven std headers + `TEST_CASE{…}` | | clean |
| the same, plus `JSON_TESTS_PRIVATE` (before *or* after `json.hpp`) | | clean |
| the same, plus the `__has_include` block | | clean |
| the same, plus the comment and **both suppress macros** | | **fails** |
| `#include "doctest_compatibility.h"` + both suppress macros + a function | | clean |

So the failure needs **doctest + nlohmann/json + the seven std headers + doctest's two pragma macros in one TU**: it is
a state interaction rather than one construct, and the next step is to shrink it by one include at a time from that
failing probe (the script is the one that produced the table: a Python loop over probe files with one CDB entry each).

### The root cause of the json failures: hedley's macros are not in the cook's table

Once `cpp_inspect rendering` existed, the failing unit could be read instead of guessed: `unit-conversions1.cpp` cooks
to **240 434** bytes and parses with exactly **eight** errors, all `expected ; after expression`, all at three
places — the bodies of `equal_general`, `less_general` and `equal_leaf`:

```text
… noexcept { JSON_HEDLEY_DIAGNOSTIC_PUSH JSON_HEDLEY_PRAGMA(GCC diagnostic ignored "-Wfloat-equal")const auto lhs_type = lhs .type(); …
```

`JSON_HEDLEY_PRAGMA(value)` is `_Pragma(#value)` (`include/nlohmann/thirdparty/hedley/hedley.hpp:742`), an operator
with **no `;`**, so a parser that reads it as a call expression reports the error against the *declaration after it* —
which is exactly what the eight errors are. Two separate defects then:

* **the cook leaves hedley's function-like macros unexpanded** — the rendering still spells `JSON_HEDLEY_PRAGMA(…)`,
  `JSON_HEDLEY_DIAGNOSTIC_PUSH` and `JSON_HEDLEY_UNLIKELY(…)` (61 occurrences) — so whatever the parser does, the
  stream it reads is not the compiler's. **And it is not specific to the failing tests**: a four-line translation unit
  whose whole content is `#include <nlohmann/json.hpp>`, one `JSON_HEDLEY_UNLIKELY(1)` and one
  `JSON_HEDLEY_PRAGMA(GCC diagnostic push)` cooks to **150 534** bytes with `JSON_HEDLEY_UNLIKELY(` **62** times,
  `JSON_HEDLEY_PRAGMA(` **4** times, and `__builtin_expect` / `_Pragma(` **zero**. So hedley's definitions never reach
  the table at all, while `__GNUC__` does (a fixture printing `gnuc_seen` proves the guards run). The next question is
  the one line that decides it: `hedley.hpp:17` is `#if !defined(JSON_HEDLEY_VERSION) || (JSON_HEDLEY_VERSION < 15)` —
  if the cook already holds `JSON_HEDLEY_VERSION` when it reaches the file, the whole of hedley is skipped, and the
  includer that gets there first is `detail/macro_scope.hpp:12`, ahead of `json.hpp:88`. That is where the next
  session starts.
* **the parser had no rule for a pragma operator between statements** — fixed here (`stats.rs`,
  `a_pragma_operator_between_statements_is_a_statement`), which is the *expanded* form hedley and doctest produce;
  `FORMAT_VERSION` 47 → 48 for the grammar change.

### `--cook` drained eight includes, not the closure — and hedley contributes no tokens at all

Two more facts, both from the `rendering` command:

* **Five test files had no reading, and the tool was the reason.** `--cook` asked for the reading and then called
  `catch_up`, which reads the file and **eight** of its includes — the slice a hover needs — while a cook needs every
  include the walk will enter, or `cooking_materials` refuses. `unit-json_view.cpp` answered "has no reading to
  diagnose" and yet `rendering` built its 206 680-byte text. The flag now drains until `needs_catching_up` is false,
  and the measurement answers **126 of 126** files (it was 121) with **95** `syntax-error` — the earlier 92 plus the
  three the five unread files were hiding.
* **`hedley.hpp` reaches the cook as nothing.** In the four-line unit's 150 534-byte rendering, `JSON_HEDLEY_UNLIKELY(`
  appears 62 times, and `JSON_HEDLEY_VERSION`, `JSON_HEDLEY_GNUC_VERSION`, `JSON_HEDLEY_VERSION_ENCODE` and the word
  `hedley` appear **zero** times — the header is either not found (no `include-not-found` finding mentions it, so it is
  found) or its whole body is skipped by its first line, `#if !defined(JSON_HEDLEY_VERSION) || (JSON_HEDLEY_VERSION < 15)`.
  That is the next thing to read: who defines `JSON_HEDLEY_VERSION` before `detail/macro_scope.hpp:12` reaches the
  file, and whether the walk marks a header read and drops its second, *first* contribution.

  **Three narrower facts, from three more four-line units** (each written into a file that is in the database, under
  its own entry, with its original text restored afterwards):

  * including `hedley.hpp` **directly** still leaves `JSON_HEDLEY_UNLIKELY(` unexpanded, while its directives *are*
    evaluated — the 169-byte rendering is exactly `#pragma once`, `_Pragma("GCC diagnostic push")`,
    `#pragma GCC diagnostic ignored "-Wvariadic-macros"`, `_Pragma("GCC diagnostic pop")`, which is the whole of what
    that header writes as tokens. So the file is walked end to end and only its `#define`s fail to arrive;
  * `#if` with a **function-like macro** in it evaluates correctly (`#define V(major) …` / `#if V(3)` takes the true
    branch), and so do `defined(__GNUC__) && CHECK(3,0,0)` and `CHECK(3,0,0) || defined(__clang__)`;
  * `__GNUC__` and `__cplusplus` are in force (`gnuc_seen` / `cpp_seen` survive).

  So the defect is narrowed to **`#define` collection inside that header's guard chain** — not the condition
  evaluator, not the compiler's predefined macros, not the walk.

### The first real fix: `Unknown || true` is true — the condition evaluator was not conservative, it was wrong

`hedley.hpp`'s definitions are guarded by

```c
#if (JSON_HEDLEY_HAS_BUILTIN(__builtin_expect_with_probability) && !defined(JSON_HEDLEY_PGI_VERSION)) \
    || JSON_HEDLEY_GCC_VERSION_CHECK(9,0,0) || JSON_HEDLEY_MCST_LCC_VERSION_CHECK(1,25,10)
```

`__has_builtin` is a question about the compiler and the cook answers it `Unknown` — truthfully, it has no table of
builtins — and `JSON_HEDLEY_GCC_VERSION_CHECK(9,0,0)` is a fact (GCC 16). The evaluator answered the **whole
disjunction** `Unknown` because its left operand was, and the cook treats an undecidable region as not compiled; so
`JSON_HEDLEY_LIKELY` and `JSON_HEDLEY_UNLIKELY` were never defined, and every `CHECK` inside a `TEST_CASE` in
nlohmann/json's tests became ``expected `;` after expression``.

**`Unknown || true` is true whichever way the unknown falls, and `Unknown && false` is false** — the fix
(`condition.rs`, both operators). What made it easy to miss is that the short-circuit arms *look* conservative: they
never guess a value. They were guessing a branch instead. Four fixtures in
`crates/cpp_toolchain/tests/suite/conditions.rs` pin the rule, including the hedley shape; `FORMAT_VERSION` 48 → 49.

Measured after it: a translation unit that includes `hedley.hpp` **directly** now expands `JSON_HEDLEY_LIKELY(1)` to
`__builtin_expect(!!(1), 1)` (it did not before), and the four-line probe goes from unexpanded to 167 bytes of exactly
what the header writes.

### The two fixes that took the json suite from 95 syntax errors to 60, and what is left

1. **A pragma operator is a statement `parse_stat` knows about.** `CHECK_THROWS_WITH_AS` expands to
   `_Pragma("GCC diagnostic ignored \"-Wuseless-cast\"")static_cast<void>(…)` inside a `try` block; the arm added
   earlier was in `parse_statement_body`, which a **compound statement's items do not go through**. Reading the operator
   as an expression left the `static_cast` with no terminator — 63 findings in `unit-conversions1.cpp` alone, and that
   file now reports **0**.
2. **`__LINE__` and `__COUNTER__` are answered at the call site.** A compiler answers them from the position of the
   *use*, so no table read from its predefines has them; the cook writes them into each run before expanding it. Without
   this, doctest's `DOCTEST_CAT(x, __LINE__)` pasted the **name** onto `x` and every capture became the same identifier
   `DOCTEST_CAPTURE___LINE__` — 86 `redefinition` findings, every one of them ours.

**And a latent defect the work surfaced**: `simplify-boolean-expr` read the condition's spelling from the **rendering**
(`node_text`), so it depended on the renderer's spacing; a separator change made it silent and its own test failed with
"the file produced []". It now reads `node_written_text` — the range mapped back through the view into the file — with
whitespace collapsed. Any rule that judges a *spelling* should use that and not `node_text`.

**What is left in the json measurement** (126 of 126 files answered):

| Code | Count | Standing |
| --- | --- | --- |
| `redefinition` | 86 | **The expansion is fixed**: `__LINE__`/`__COUNTER__` are answered from the **outermost** call site, so doctest's macros render `DOCTEST_CAPTURE_530` / `DOCTEST_CAPTURE_OTHER_530` rather than the literal `DOCTEST_CAPTURE___LINE__`. The finding's message still names `DOCTEST_CAPTURE_2` — and the name is in the **binding**, i.e. in the summary the index built. The rendering `diagnostics` parses and the rendering `rendering` answers are **two different texts**: the fresh one has `DOCTEST_CAPTURE_530` (line 530, per-file text), the index's has `_2`, because the index path cooks against the **unit's concatenated text**, where `__LINE__` counts lines of the concatenation rather than of the file. So the remaining item is which *text* the index cooks against, not the expansion — `__LINE__` must be numbered in the file the token was written in. |
| `syntax-error` | 60 | three files — `unit-json_view_from_json.cpp`, `unit-regression3.cpp`, `unit-udt_macro.cpp` — all `expected `;` after expression`. |
| `use-override` | 28 | doctest's own code: `virtual` without `override`. A style rule firing on a third-party header. |
| others | 16 | `return-type-mismatch` 4, `assign-type-check` 3, `access-invisible` 2, `implicit-bool-conversion` 2, `include-not-found` 2, `undefined-name` 2, `macro-parentheses` 1. |

And the **measurement's own database** was incomplete in a way worth recording: nlohmann/json's tests inherit three
definitions from `tests/CMakeLists.txt` (`DOCTEST_CONFIG_SUPER_FAST_ASSERTS`, `JSON_TEST_KEEP_MACROS`,
`JSON_TEST_USING_MULTIPLE_HEADERS=1`) and `TEST_DATA_DIRECTORY` comes from a header **generated** by
`cmake/download_test_data.cmake`. Without them, `TEST_DATA_DIRECTORY "/nativejson-benchmark/canada.json"` is an
identifier followed by a string literal — 97 of the 157 syntax errors that were left were the build's, not the parser's.
`target/bench/json_cdb.py` now writes the database the project would have built.

### Still open, and now precisely: **a header included from a header contributes nothing**
### Still open, and now precisely: **a header included from a header contributes nothing**

**The reproduction, with no nlohmann code in it** (`E:\corpora\json-probe`): `probe.cpp` includes `x.hpp`, `x.hpp`
includes `y.hpp`, `y.hpp` defines `Y_MACRO`. All four quoted/angle combinations give a 33-byte rendering
(`#pragma once` and the translation unit's own line) and `Y_MACRO` never expands; the same `y.hpp` included **by the
translation unit** works in all four spellings. `y.hpp` declaring something instead of defining a macro is dropped the
same way. System headers behave alike: `<utility>` at depth one contributes (125 bytes of live tokens), at depth two
nothing (27 bytes).

**And the cook's shape explains it**: `cook_with` is `cook_with_search(…, None, "")` — a cook has **no header search**,
so it can only inline an include whose reading the *walk* already stored. `catch_up` fills that store for the file and a
slice of its includes, which is depth one. In the three-file project, `--index --all-files` (which seeds and indexes the
whole root) makes the depth-two include arrive — 41 bytes, macro expanded — so the missing piece is a **reading for the
included file**, not a grammar rule.

What does not follow is that seeding fixes everything: nlohmann/json **is** scanned whole (171 files, `include/` and
`tests/thirdparty/` included) and still drops `doctest.h` and `hedley.hpp`. So the next question is narrower than "the
walk": why a file the scan holds has no reading for the walk — an unread include at depth two, a summary without a
rendering, or the walk's own `already read` bookkeeping. `cpp_inspect rendering --limit 1` answers a file's rendering
size in one call, and a 27-byte answer for a header with 200 lines is the signature.

After the condition fix the json measurement stands at **126 of 126 answered, 95 `syntax-error`** — `unit-conversions1.cpp`
went from eight to six (its two hedley ones are gone), and the rest are the doctest shape above.



Through `#include <nlohmann/json.hpp>` the same probe is still unexpanded, and a three-file probe in a project of its
own (`E:\corpora\json-probe`) reproduces the reason without any of nlohmann's code:

```text
tests/probe.cpp  #include "x.hpp"   int probe = Y_MACRO;
tests/x.hpp      #pragma once       #include "y.hpp"
tests/y.hpp                         #define Y_MACRO 42
```

| outer | inner | `Y_MACRO` expanded | rendering |
| --- | --- | --- | --- |
| quoted | quoted | **no** | 33 bytes: `#pragma once` + the probe line |
| quoted | angle | **no** | 33 bytes |
| angle | quoted | **no** | 33 bytes |
| angle | angle | **no** | 33 bytes |

and `y.hpp` included **directly** by the translation unit works in all four spellings (`Y_MACRO` → `42`). A `y.hpp`
that declares something instead of defining a macro is dropped the same way, so it is not "a header of only defines".
`hedley.hpp` through `macro_scope.hpp` is the same shape (47 bytes, `JSON_HEDLEY_VERSION` undefined), which is why the
json measurement still reports its 95 errors: the depth-2 includes of the whole library are gone from the rendering,
and the parser is being asked to parse what is left. **That is the next fix** — in the include walk, a file entered at
depth ≥ 2 from a project header.

### The probe files were measured on a rendering of nothing — the bisect below is void


Trying to shrink the failing shape to a fixture, every probe created as a new `.cpp` under `tests/src` answered from a
**114-byte rendering** — the file's own tokens with the include closure collapsed to one `using` line — while
`unit-constructor1.cpp` answers from **254 299 bytes** and `unit-binary_formats.cpp` from 245 659. The probe files are
in the same database, with the same `-I`; the difference is the file itself, and a cold cache does not change it. So
every probe result recorded above ("clean", "fails") was a statement about a rendering with no doctest and no library in
it, which is why probes contradicted each other and why `TEST_CASE` was reported at one place and not another.

What survives: the **measurement** itself, because it reads real translation units — the 19 failing files cook to
200–250 kB renderings, so their 92 syntax errors are errors of parsing a fully cooked TU. What does not survive: the
bisect table. The next step is to bisect **inside a copy of a real failing TU** (rename `unit-conversions1.cpp`'s text
into a file that is already in the database, rather than adding new files to it), and to find out why a *new* file
cooks without its closure — the summary cache key does not include the compilation command, or the walk gives up on a
new path. `cpp_inspect rendering` (added here) is the command that shows it: `--limit 1` answers the size.

### Two tool fixes the measurement needed


* **`--cook` cooked nothing.** `Session::cook` answers `None` for a file the index has no summary of, and the flag did
  nothing for a file that was not also indexed: `diagnostics` then failed with "has no reading to diagnose" on every
  nlohmann test file. The flag now asks for the file's reading and drains it (`want_cooked_reading` + `catch_up`)
  before cooking.
* **`sweep` measured a different reading than a client's pull.** The sweep implied `--index`, and
  `Session::diagnostics` *prefers a stored cooked reading* over the one just cooked — which is the index's reading,
  built without the file's own include path in a header-only project: doctest's
  `CHECK_THROWS_WITH_AS(…, json::out_of_range&)` stayed unexpanded and the sweep reported **2 457** syntax errors over
  151 files where the per-file path reports **92 over 19**. The sweep now seeds the files (`--all-files`) without
  indexing them, and cooks each one the way `--cook` does. The number a `sweep` reports is now the number a pull
  reports.

## The review's P1 list: what is confirmed, and what is fixed

`docs/parser-toolchain-review.md` was checked against the tree rather than taken on its word. Three of its findings were
reproduced and fixed in this round; the rest were checked by reading the code it points at, and its line references and
claims held everywhere I looked.

**Fixed — P1-1, local type names** (`091fc1ba`). `TypeNames` kept one entry per name with the brace depth it was
written at, and visibility was `declared_at <= depth`, so a `typedef` in one function was a type name inside its
siblings: `Leaked()` came out as a `Declaration` with a parameter list, no `CallExpr`, no diagnostic. It is now a stack
of scopes. The review's step 2 — recording *value* bindings so a variable can hide a type — is deliberately **not**
done, and the module docs say so.

**Fixed — P1-10, MSVC's option spellings** (`0e9690f3`). `config_from_arguments` read `-I`, `-D` and `-U`, and not
`/I`, `/D` or `/U`, so a `cl.exe` database lost its include directories **and** its feature macros: conditionals took
the branch the build does not, and generated config headers were never found. The joined and separated forms are read
now, with the flags that merely begin like them written down as refusals — `/I-`, `/DEBUG`, `/D0`, `/D1`, `/D2`,
`/doc`, `/INCREMENTAL`, `/IGNOREIDL` — because `/DEBUG` read as `-D EBUG` would invent a definition and change the
branches the file is read in. The test feeds a `cl.exe`-style argument list and pins all three lists.

**Fixed — P1-9, the inferred `include/` root** (`d9f28861`). `IncludeResolver::resolve` searched the root inferred
from `include/` beside `src/` for **every** include form, though the comment above it says quoted only, and it searched
it **before** the compile command's `-I` paths. Measured in the new unit test: an angle `#include <vector>` resolved to
the checkout's own `include/vector` instead of the `-I` directory's. The guard is back and the test fails without it
(`left: include/vector`, `right: user/vector`).

**Fixed — P1-8, guards as conditions** (`67b8bb33`). `decide_visit` skipped a guarded file once it had been visited,
whatever the macro state said, so `#undef GUARDED_H` between two includes — which makes the compiler read the body again —
was skipped here; and `has_pragma_once` counted a `#pragma once` inside `#if 0`, where the compiler never sees it. The
guard now has to block *for that file* (the name is in the file's own `defines` and still defined at this include), and
only a directive at condition depth 0 counts as a pragma guard. Four tests: the `#undef` case and the inactive pragma
fail without the change, the copied-guard-name case and a live guard are the controls.

**Fixed — the flaky `cpp_ls` tests** (`a3957bf1`, `3e5cbddf`). Two causes, both test-side. The fixture helpers built
their temp directory from the **process id**, which every test in one binary shares, so one test's `remove_dir_all`
landed in another's `fs::write` (fixed with a per-binary counter). And three assertions in `latency` answer "how much
spare machine was there" rather than "what does the code do" — they failed only under a full-workspace run — so they now
run under `CPPLS_LATENCY_STRICT=1 ... -- --test-threads=1`, while the fill-time bound and the steady-state median stay
unconditional. Two full-workspace runs after both changes were clean.

**Confirmed by reading, not yet fixed** — in the order this round would take them:

| item | where | what the code does |
| --- | --- | --- |
| P1-3 | `cpp_lexer.rs:247` | `contract_assert` becomes a keyword **unconditionally**, so a C++23 identifier of that name fails to parse; `pre`/`post` are contextual and unaffected. Fixing it needs a `Standard::Cpp26` and a move of the vendor bits in `LanguageConfig` (the vendor block starts at bit 10). |
| P1-5 | `syntax/id.rs:139` | `deserialize` calls `CppKind::from_raw` and `TextRange::new` without validating either, so a malformed cache entry can panic the reader. |
| P1-6, P1-7 | `graph.rs:592` | the walk computes every directive's visibility once, before the loop that applies `#define`/`#undef`; unknown branches and unread includes do not mark the macro state uncertain. |
| P1-4 | `cpp_tree_builder.rs:219` | `balance_events` appends a `NodeEnd` for each open node at the end of the event list, which is what its own comment says it must not do. |

## curl's last three `syntax-error`s, and the two defects behind them

`lib/curl_gssapi.c:63,66` and `lib/multi.c:3516` were the only errors left in curl's `lib`, and neither was a curl
construct:

* **`gss_OID_desc Curl_spnego_mech_oid CURL_ALIGN8 = { 6, … };`** where `CURL_ALIGN8` is
  `__attribute__((aligned(8)))`. The specifier sequence takes a name as another *word of the type* when what follows it
  can still be a declarator, and `__attribute__` is an identifier — so the declarator's own name joined the type, the
  declarator was left empty, and the initializer had nothing to initialise: ``expected `;` after expression`` at the
  name. The rule now asks what follows the **attribute** (`=`, `;`, `,`, `{`, `(` means the name is the declarator;
  another name, a `*`, a `&` or `operator` means the attribute belongs to the type), which is also the shape
  `MY_API Widget __attribute__((unused)) x;` needs. `extensions_gnu` has both spellings, and the case fails without the
  fix.
* **`struct Curl_mapi_guard guard;` at `multi.c:3516`** was a **cascade**, not the construct: `multi.c` writes
  `#undef curl_multi_socket` and then defines the function of that name, and the lookup fell back to `multi.h` after
  finding the `#undef`. So the definition's own name expanded through the public macro —
  `#define curl_multi_socket(x, y, z) curl_multi_socket_action(x, y, 0, z)` — leaving `0` where the second parameter's
  type belonged: `curl_multi_socket_action(CURLM *m, curl_socket_t s, 0, int *running_handles)`. `MacroBindings` can now
  say **"undefined here"** as well as "not in this layer", and both layered lookups (`Live`, the cook's own table over
  its starting layer, and `Over`, the closure's) stop instead of falling through.

Measured with the same probes:

| corpus | before | after |
| --- | --- | --- |
| curl 8.x `lib` | 146 files, 397 findings, **3** `syntax-error` | 146 files, 404 findings, **0** `syntax-error` |
| curl 8.x `src` | 42 files, 86 findings, 0 | unchanged |
| libuv / redis / nginx | 0 `syntax-error` each | unchanged |

The findings rose by seven because the code the broken expansion used to hide is now live and read: nine more
`braces-around-statements` and one more `else-after-return`.

The cross-file `#undef` rule is verified by the two-file fixture (`target/bench/undef_header.h` and `undef_user.c`:
the use after the `#undef` is no longer expanded), **not** by a unit test — the suite's helpers cannot build a layered
table from text yet, and a same-file `#undef` never had the defect. A `rendered_with(table)` helper is the next step
if that rule needs to be pinned.

## The host wedged twice in one session — what to do when a command that cannot be slow hangs

Twice today the shell stopped answering: the second time a **literal string** (`'probe-ok'`) did not return within 20
seconds, and before that a 90-second `Get-Process` — a pure OS query — did not return either. The first occurrence was
fixed by rebooting the machine. Nothing in this repository is implicated: the shell layer could not run `echo`.

What made it expensive was not the wedge but **how long each hanging command was allowed to run**: a 10-minute timeout
on a `Get-Content` of a 700-line file spends ten minutes to learn nothing. The rules that follow from it:

* **Every command gets a timeout that its work justifies.** Trivial file/path queries: 20–60 s. A build or a sweep:
  as long as it needs, and then it runs in the background and is collected by `job_output` rather than by waiting.
* **A command that exceeds its budget is killed at once** (`job_kill`), not left to finish — and the next attempt is a
  *different* command, not the same one again.
* **Corpus files are read with the file tools** (`read`, `grep`, `glob`, which run in-process), not with `Get-Content`:
  a freshly cloned tree (nginx, curl, json) is exactly where the disk is slowest.
* **One measurement at a time.** Several concurrent `cpp_inspect sweep` processes are gigabytes and cores each; the
  numbers get slower and less comparable, and the machine is what pays for it.
* The first symptom to watch for is a command that *should* be instant not returning: stop, kill it, and check the host
  rather than retrying.



## The goal's two json defects, closed — and the numbers

Both of the objective's json items are fixed, each by a minimal reproduction and a separate commit:

* **`__LINE__`/`__COUNTER__` numbered in the file that wrote them.** Two defects, not one: the `__COUNTER__` count was
  per **stream segment** instead of per translation unit (`732e4352`), and a `__LINE__` written in a macro body expanded
  during an **argument prescan** answered the line of the *reconstruction* — two bytes of text, so a line like 2 —
  because the call had not been pushed onto the stack yet (`93e48780`, with the run's own span as the fallback). The
  names are now the ones a compiler would make: `DOCTEST_CAPTURE_530`, `DOCTEST_CAPTURE_OTHER_530`.
* **The 60 `syntax-error`s.** One expander defect: a macro's replacement was rescanned with that macro painted even for
  tokens that came **after** the replacement — a callee name ending the replacement, called by arguments written after
  it, which is how json's `NLOHMANN_DEFINE_TYPE_INTRUSIVE` is built. C11 6.10.3.4 says *"not including the rest of the
  source file's preprocessing tokens"*, and the fix makes that true (`25c1ece4`, with a fixture that fails without it and
  the standard's own nested-macro example beside it).

| measurement | before this round | after |
| --- | --- | --- |
| nlohmann/json, 126 test files | 190 findings: 86 `redefinition`, 60 `syntax-error` | **47 findings: 0 and 0** |

What remains there is 30 `use-override` (doctest's own code), 4 `return-type-mismatch`, 3 `assign-type-check`,
2 `access-invisible`, 2 `implicit-bool-conversion`, 2 `include-not-found`, 2 `undefined-name`, 1 `macro-parentheses`,
1 `missing-return`.

And the C corpora, all re-measured with the final binary (same probes, `sweep`):

| corpus | measured | skipped (no command) | findings | `syntax-error` |
| --- | --- | --- | --- | --- |
| libuv 1.53.1 `src` | 55 | 54 | 349 | **0** |
| redis 8.x `src` | 127 | 96 | 1116 | **0** |
| curl 8.x `lib` | 146 | 188 | 397 | **3** |
| curl 8.x `src` | 42 | 45 | 86 | **0** |
| nginx `src` (a real `configure --crossbuild=win32` database) | 128 | 282 | **33** | **0** |

nginx's 33 are `macro-parentheses` 23, `else-after-return` 5, `unused` 3, `include-not-found` 2 — the same hint classes
as the other C projects. The three curl `syntax-error`s are `lib/curl_gssapi.c:63,66` and `lib/multi.c:3516`
(`struct Curl_mapi_guard guard;`): the next thing to triage, and not part of this goal.

## The C corpora, re-measured after the cook and parser work — and the measurement itself was wrong three ways

The condition evaluator now decides `#if` the way the standard does (`Unknown || true` is true, the rule that fixed
hedley), the cook answers `__LINE__`/`__COUNTER__`, and includes are read through the whole closure. All three change
what a rendering contains, so every C number had to be taken again — and taking them exposed three defects **in the
sweep**, each of which had been quietly changing the numbers:

* **A file the compilation database does not name was measured against no configuration at all.** libuv's database has
  80 commands and no `aix.c`, `ibmi.c`, `kqueue.c` or `linux.c` — other platforms' sources — and cooking one of them
  invents include paths: that was the "27 syntax errors" the first re-measurement reported. Such files are now counted
  (`not_in_the_database`, with the database's path and the first examples in the answer) and not measured.
* **The loop read eight includes, not the closure.** `catch_up` is a hover's slice; libuv's `signal.c` reported a syntax
  error there and none through `diagnostics --cook`. The sweep now takes the same whole-closure walk as `--cook`.
* **The project list is not the build's file list.** A session's `project_files` is what the scan found; a
  `compile_commands.json` is the list of translation units — on curl the scan's list held only the 188 headers, so the
  sweep measured zero files. The union is taken, filtered to the extensions this analysis reads (a CMake database on
  Windows also lists `libcurl.rc`, which is not C), and `--including` compares one separator on both sides (a database
  writes `E:/corpora/curl/lib/url.c`, a scan writes `E:\corpora\curl\lib\url.c`, and the filter had been matching one
  half of the project and not the other).

With those fixed:

| corpus | files measured | not in the database | findings | of which `syntax-error` |
| --- | --- | --- | --- | --- |
| libuv 1.53.1, `src` | 55 | 54 | **349** | **0** |
| redis 8.x, `src` | 127 | 96 | **1116** | **0** |
| curl 8.x, `lib` | 146 | 188 (headers) | **397** | **3** |
| curl 8.x, `src` | 42 | 45 (headers) | **86** | **0** |

The findings are the hint categories these projects have always produced: `braces-around-statements` (831 in redis,
310 in curl's lib), `else-after-return`, `macro-parentheses`, `unused`, and `include-not-found` where a POSIX header is
absent on Windows. The three `syntax-error`s left are all in curl's `lib`, and are the next triage:

| file | line | construct |
| --- | --- | --- |
| `lib/curl_gssapi.c` | 63, 66 | `expected `;` after expression` |
| `lib/multi.c` | 3516 | `expected primary expression` — a `struct Curl_mapi_guard guard;` local in `curl_multi_socket` |

## libuv, re-measured after the cook and parser work — **and it went from 0 syntax errors to 27**

The same sweep (`cpp_inspect --project E:\corpora\libuv --file … sweep --including libuv\src`), the same corpus, the same
database. The number moved because the **condition evaluator now decides `#if` the way the standard does** (`Unknown ||
true` is true — the rule that fixed hedley), and libuv's platform branches are full of
`#if defined(X) || …` conditions whose left side is a macro nobody defined. A branch that used to be **skipped** is now
**compiled**, so the parser is handed code it had never seen — and cannot read:

| file | line | first error |
| --- | --- | --- |
| `src/unix/aix.c` | 754 | `expected primary expression` |
| `src/unix/ibmi.c` | 543 | `expected primary expression` |
| `src/unix/kqueue.c` | 496 | `expected primary expression` |
| `src/unix/linux.c` | 837, 867 | ``expected `;` after expression`` |

Every one of them is `struct file: first error` on a **platform file** — AIX, IBM i, kqueue, Linux — which the old
evaluation had left out of the rendering. The shape is the same in each: a function **head** has lost its tokens and the
error lands on the next keyword (`void do{…}while(0)`, where `do` is libuv's `UNREACHABLE()`):

```text
… avg[2]; } void do{(void)((!!(0 && "unreachable code"))||(_assert("0 && \"unreachable code\"","",3),0));abort();}while(0) { }
```

So this is **not** a regression in the analysis but a **parser gap that the fix exposed** — and the next step is the same
one the C work has always taken: take the smallest of those (aix.c's, 15 790-byte rendering), find the construct, fix it,
and only then believe the number.

**The aix.c construct, located**: the rendering holds

```text
… avg[2]; } void do{(void)((!!(0 && "unreachable code"))||(_assert("…","",3),0));abort();}while(0) { } int uv_fs_event_init(…
```

and the source is `aix.c:845`:

```c
void uv__fs_event_close(uv_fs_event_t* handle) {
#ifdef HAVE_SYS_AHAFS_EVPRODS_H
  uv_fs_event_stop(handle);
#else
  UNREACHABLE();
#endif
}
```

The **function head is missing from the rendering** (`uv__fs_event_close` appears once in the whole 15 790-byte text, and
not here) while the body's `UNREACHABLE()` — a `do{…}while(0)` — is there, so the parser reads `void do{…}while(0) { }`.
A head that is live in the file and gone from the stream, with a directive immediately after it, is a **slicing defect in
the cook**, not a grammar gap: the next fix is here, and it is the first thing the goal's C half needs. Until then the honest statement about libuv is *"0 syntax errors under the old
condition evaluation; 27 in code that evaluation was hiding"*.

## curl 8.x — the third C project, and the cleanest so far

Cloned at `E:\corpora\curl`, configured with its own CMake and MinGW's gcc — a **real** database, not a synthesized one:

```text
cmake -S . -B build -G Ninja -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
      -DCMAKE_C_COMPILER=…/w64devkit/bin/gcc.exe
      -DCURL_USE_OPENSSL=OFF -DCURL_USE_LIBPSL=OFF -DCURL_USE_LIBSSH2=OFF
      -DUSE_LIBIDN2=OFF -DCURL_ZLIB=OFF -DCURL_BROTLI=OFF -DCURL_ZSTD=OFF
      -DBUILD_TESTING=OFF -DBUILD_EXAMPLES=OFF
```

255 entries (`lib/` 210, `src/` the rest); `compile_commands.json` copied beside the project root, which is where the
session looks for it.

| Measurement | files | findings | errors |
| --- | --- | --- | --- |
| `lib/` | 152 | **68** | **0** |
| `src/` (the command-line tool) | 44 | **13** | **0** |

No `syntax-error`, no `undefined-name`, no type finding — in either half. All 81 findings are warnings, and each class is
one of the three the earlier projects named:

| Code | Count | What it is |
| --- | --- | --- |
| `missing-include` | 56 + 13 | "`Curl_easy` is declared in `curl_trc.h`, which this file does not include" — in a **header** (`lib/cf-h1-proxy.h`) that every translation unit includes *after* `curl_setup.h`. Plausible "include what you use", noisy for curl's header style. The attribution ("declared in `cf-https-connect.h`") also looks arbitrary for a name several files declare — the *first* declaring file the index happened to hold. |
| `include-not-found` | 10 | **Platform headers, not this platform**: `ConditionalMacros.h` (macOS), `<sys/socket.h>`/`<gskssl.h>`/`<qsoasync.h>` (OS/400's `setup-os400.h`), `<pwd.h>`/`<unixlib.h>`/`<openssl/evp.h>` (VMS's `setup-vms.h`). Those files are guarded out of a Windows build; the sweep scans them because they are `.h` in `lib/`. |
| `macro-parentheses` | 2 | `#define argv_item_t char *` and `#define curl_thread_t …` — macros that stand for a **type**. Parenthesising the replacement list would break the declaration, so this is a rule refinement (a body that is a type is not an expression), not a defect in curl. |

That is the third C project measured end to end, and the first with **no errors at all** on its first run — after
`f53b5bc6`, `db1bb5c6` and the two rule fixes above. It is also the strongest evidence so far for the claim the work
started from: what a C project reports is dominated by whether its build configuration is real, and by then by style
warnings, not by the analysis failing to read the code.

## redis 8.x — the second C project, and the first measured with a database

131 files (`src/*.c`), a database synthesized from `src/Makefile`'s own flags: `gcc -std=gnu11 -pedantic
-DREDIS_STATIC= -D_GNU_SOURCE -DUSE_JEMALLOC` and the seven `-I../deps/…` paths the Makefile composes. Measuring on
Windows with a MinGW toolchain is a *platform* choice, and it is why the remaining `include-not-found` findings are
about POSIX headers rather than about redis.

| Measurement | errors | warnings | hints |
| --- | --- | --- | --- |
| scan only, no database (§8.1 below in the old numbering) | 427 (4 syntax) | 573 | 975 |
| with the synthesized database | 38 (2 syntax) | 181 | 850 |
| … and C's string-literal rule (`e023c54e`) | 19 (2 syntax) | 181 | 829 |
| … and a parenthesised function declarator (`db1bbcd3`) | 17 (0 syntax) | 181 | 902 |
| … and two more probes of the same kind (`f0e2c1d4`-era): a multi-declarator's type, the assignment idiom | **16 (0 syntax)** | 181 | 902 |

### 1. A macro parameter may be spelled with a keyword — `d0de833f`

`#define createBoolConfig(name, alias, flags, config_addr, default, is_valid, apply)` — `default` is a keyword, and
`parse_parameters` accepted only `Identifier`s. The parameter list was cut there, the rest of it became the macro's
**body**, and the macro became *object-like*: redis's whole `standardConfig static_configs[]` table stayed unexpanded
text. **952 of the file's 1002 syntax errors were `src/config.c` alone.** The preprocessor's grammar has no keywords
(C11 6.10.3), so a parameter is judged by the same rule the macro's own **name** already used (`could_be_a_macro_name`,
which exists for `#define true 1`). Syntax errors 501 → 4; errors 904 → 427.

Fixture: `preprocessor_against_the_compiler::a_parameter_spelled_with_a_keyword_expands_as_the_compiler_says`, compared
against `gcc -E`.

### 2. A header the client calls C is not judged by C++ rules — `a2676c79`

`cstyle-cast`, 73 findings, **all in `.h` files**: `(int)p` is a C-style cast in C++ and *the* cast in C. A header's
family is deliberately nobody's guess (it is included by whichever language includes it), so a `.h` took the
project-wide standard — `c++23` in an editor's defaults — and `is_cpp_only`'s rule "not told is not C" did the rest.
`didOpen`'s **`languageId`** is the statement nothing else makes; the server was discarding it. It is kept per open
document and used for a file the compilation database does not name. `cstyle-cast` 73 → 0.

Fixture: `cpp_ls/tests/a_header_language.rs` — the same header, once with `languageId: "cpp"` (finding present), once
with `"c"` (nothing), both through the real server.

### 3. C's string literal is `char[]`, not `const char[]` — `e023c54e`

Every one of twenty findings was one pair: a string literal meeting a `char *`. In C the literal's type has **no
`const`** (C11 6.4.5p6), so `char *p = "…";` is exact; the type layer spells a literal `const char*` in both languages
because a spelling is one answer for two. The language is known at the check layer and the expression is in hand there,
so the three rules that ask a conversion question consult `CheckContext::a_string_literal_in_a_char_pointer_position`.
Only a literal: a `const char *` variable dropped into a `char *` is a discarded qualifier in C too. 20 → 0, and the
C++ half is untouched.

Fixture: `assign_type_check::a_string_literal_in_a_char_pointer_position_is_c_only` (silent in C, reported in C++).

### 4. A calling convention is not the pointer — `e023c54e`

MinGW writes `char *__cdecl strchr(const char *, int)`; the tree is
`Declarator { Declarator { PointerType { Star, Identifier "__cdecl" } }, NameExpr "strchr", … }`. The return spelling
was assembled from the **text** before the name — `*__cdecl` — which the "purely operators" filter refused, so
`strchr` recorded `returns: char`. Every caller then read it as returning a `char`, and
`char *pch = strrchr(path, '/')` was reported as a conversion that does not exist. The operators are now read from the
**tree** (`*` per `PointerType`, `&` per `ReferenceType`), so a word beside the star cannot hide it; a parenthesised
return type (`int (*)(int b)`) still wins, and the text keeps the shapes the tree says nothing about.

Fixture: `declarations::a_function_records_what_it_returns_and_nothing_else_does`'s `conventional` case.

### 5. The two syntax errors: a macro standing for a variable — fixed

Both files failed at their first declaration after the includes, with `expected ), but get identifier`:

```
sentinel.c:26      extern char **environ;
server.c:2620      extern char **environ;
```

`extern char **environ;` parses on its own in either dialect, and the whole file read clean through `cpp_inspect`, so
the line was not the problem. The **rendering** was: `CPPLS_DUMP_RENDERING_DIR` showed the cooked stream starting with

```c
extern char **(*__p__environ());
```

which is MinGW's own `#define environ (*__p__environ())` expanding — correctly — inside redis's declaration. The shape
is a **parenthesised declarator whose inner declarator is a function**, and `gcc -std=gnu11 -fsyntax-only` accepts it.
Our parser did not: `a_parenthesised_declarator_with_a_name_follows` claims a group only when it is `( <op> name )`,
so this group was read as an *abstract* pointer-to-type, whose reading wants a parameter list after the `)` — there is
none, the declaration failed, and the damage landed on the line after the includes (which is what made it look like the
includer's fault).

Two changes, both in `types/declarators.rs`:

* the guard also claims `( <op> name (` — a group whose inner declarator is a function (`(*f())`), while `(*f)(int)`
  stays on the arm that already had it;
* `parse_parenthesised_declarator` reads that inner name's own parameter list before the group's `)`, so the suffix
  belongs to the function inside rather than to the pointer outside.

Measured: redis **19 → 17 errors, 2 → 0 syntax errors** (hints 829 → 902 — the two files now parse in full), and libuv
unchanged at **0**. The C++ shapes are untouched: `int (*fp)(int)`, `int (*maker(int a))(int b)`,
`void (*signal(int, void (*)(int)))(int)` and `typedef int (*binary_op)(int, int)` all parse, and the parser's
501 tests pass.

Fixture: `char **(*p__environ2(void));` and `char **(*f());` in `target/bench/environ2.c`'s shape — a parser test is
added beside `a_second_parameter_list_after_a_macro_type_is_one_declarator`.

### 6. Two rules and one reader, from the same measurement — fixed

Three defects, all found by reading the sweep's output rather than by a report:

* **`return-type-mismatch` on `src/dict.c:1156`.** `unsigned long long integers[6], hash = 0;` followed by
  `return hash;` was reported as returning a value that does not convert — because the reader that answers "the type
  this name was declared with" took the declaration's **first** `InitDeclarator`, so `hash` inherited `integers[6]`'s
  array. `declarator_declaring` now picks the `InitDeclarator` **that holds the name** (`sema/types.rs`), and the same
  reader answers `unsigned long long` for `hash` and `int` for `a` in `int a = 0, b[6];`. Fixture
  `types::each_declarator_of_one_declaration_has_its_own_type`.
* **`assignment-in-condition` on redis's own idiom.** `if ((ln = listNext(&li)))` — the assignment is wrapped in its
  own parentheses, which is exactly how C says "this one is deliberate"; gcc's `-Wparentheses` does not warn for it
  either. The rule knew about `((x = f()) != 0)` (a comparison) but not about the bare doubled parenthesised
  assignment, and reported **13** of them. It is now silent for that spelling, and `if (x = f())` is still reported.
* **The `dangling-return` one is *not* fixed, and the attempt was reverted.** `src/t_hash.c:4192` returns
  `&dictExpireMeta->elemExpires` where `dictExpireMeta` is a local **pointer**: the referent is the object the pointer
  points at, not the local, so the finding looks wrong. `CppMemberExpr::is_arrow()` is the right question and the
  change was written — but the rule still reported the shape through the cooked reading, and a `codes_of` fixture did
  not reproduce the case at all (a local pointer to a struct, `&meta->expire`). A change that no test and no
  measurement shows is a change not made: both the change and its test were removed, and this stays an **open** item
  whose next step is to instrument `origin_of` on the cooked tree of `t_hash.c`.

After the first two: redis 8.x src, 222 files — `assignment-in-condition` 12 → **0**, `return-type-mismatch` 1 → **0**.
The probe that produced the earlier error counts is deleted (§7 of `inspect.md`), so the tally below is `sweep`'s: 222
files, and the Error-severity codes are `undefined-name` 23 and `dangling-return` 1.

### What is left, and whose defect it is

### 6. Two rules and one reader, from the same measurement — fixed

Three defects, all found by reading the sweep's output rather than by a report:

* **`return-type-mismatch` on `src/dict.c:1156`.** `unsigned long long integers[6], hash = 0;` followed by
  `return hash;` was reported as returning a value that does not convert — because the reader that answers "the type
  this name was declared with" took the declaration's **first** `InitDeclarator`, so `hash` inherited `integers[6]`'s
  array. `declarator_declaring` now picks the `InitDeclarator` **that holds the name** (`sema/types.rs`), and the same
  reader answers `unsigned long long` for `hash` and `int` for `a` in `int a = 0, b[6];`. Fixture
  `types::each_declarator_of_one_declaration_has_its_own_type`.
* **`assignment-in-condition` on redis's own idiom.** `if ((ln = listNext(&li)))` — the assignment is wrapped in its
  own parentheses, which is exactly how C says "this one is deliberate"; gcc's `-Wparentheses` does not warn for it
  either. The rule knew about `((x = f()) != 0)` (a comparison) but not about the bare doubled parenthesised
  assignment, and reported **13** of them. It is now silent for that spelling, and `if (x = f())` is still reported.
* **The `dangling-return` one is *not* fixed, and the attempt was reverted.** `src/t_hash.c:4192` returns
  `&dictExpireMeta->elemExpires` where `dictExpireMeta` is a local **pointer**: the referent is the object the pointer
  points at, not the local, so the finding looks wrong. `CppMemberExpr::is_arrow()` is the right question and the
  change was written — but the rule still reported the shape through the cooked reading, and a `codes_of` fixture did
  not reproduce the case at all (a local pointer to a struct, `&meta->expire`). A change that no test and no
  measurement shows is a change not made: both the change and its test were removed, and this stays an **open** item
  whose next step is to instrument `origin_of` on the cooked tree of `t_hash.c`.

After the first two: redis 8.x src, 222 files — `assignment-in-condition` 12 → **0**, `return-type-mismatch` 1 → **0**.
The probe that produced the earlier error counts is deleted (§7 of `inspect.md`), so the tally below is `sweep`'s: 222
files, and the Error-severity codes are `undefined-name` 23 and `dangling-return` 1.

### What is left, and whose defect it is

| What | Count | Whose |
| --- | --- | --- |
| `include-not-found` | 92 | **The measurement's**: `<sys/socket.h>`, `<poll.h>`, `<sys/epoll.h>` are POSIX headers, and redis is a Unix program measured with a MinGW toolchain. On Linux these resolve. |
| `macro-parentheses` | 58 | redis's own style (`atomicvar.h`); the rule is a Warning and the bodies really are unparenthesised. |
| `missing-include` | 19 | Follows from the same absent headers. |
| `undefined-name` | 15 | `ae_epoll.c`/`ae_kqueue.c`/`ae_evport.c` are **platform files** this build would not compile; my database listed every `src/*.c`. Correct findings about the wrong platform. |
| `assignment-in-condition` | 12 | gcc's `-Wparentheses` says the same. Not triaged line by line. |
| `syntax-error` | 0 | Fixed above: a macro standing for a variable made a parenthesised function declarator out of `extern char **environ;`. |
| `return-type-mismatch` | 1 | `src/dict.c:1156` — **open, diagnosed**: `unsigned long long integers[6], hash = 0;` followed by `return hash;`. The **fact** for `hash` is right (`unsigned long long`, `cpp_inspect facts`), while the **expression type** of `hash` is `unsigned long long[6]` (`cpp_inspect type --needle "return hash;" --shift 8`) — the second declarator inheriting the first one's array suffix somewhere between the scope tree and the type engine. Reproduced minimally in four lines; the search is where a name's type is answered, not where facts are built. |
| `dangling-return` | 1 | `src/t_hash.c:4194` — a pointer into a local struct member. Plausible; not triaged. |

---

## How to reproduce any number here

The probes these numbers were taken with are **gone**; the instrument is `cpp_inspect` (protocol:
[`inspect.md`](inspect.md)). A project's tally, by code:

```text
cpp_inspect --project E:\corpora\redis --file src/dict.c sweep
cpp_inspect --project E:\corpora\libuv --file src/win/core.c sweep
```

A single question — the one that found most of the defects above:

```text
cpp_inspect --project E:\corpora\redis --file src/dict.c type --needle "return hash;" --shift 8
cpp_inspect --project E:\corpora\libuv --file include/uv.h   facts --name uv_translate_sys_error
```

`--code` and `--including` narrow a sweep; `batch` asks many questions in one process, which is what a whole-project
investigation actually does. A project with no `compile_commands.json` is scanned, so the first number for a new
project is always "what the analysis says when nobody has told it anything" — the redis table above is what the second
number looks like once a database from the project's own Makefile is supplied.

## libuv 1.53.1 — the first C project


The first C project measured end to end, because "usable for C" is a claim about a program rather than about a grammar.
libuv is the right first subject: C11, `typedef struct` and macros all the way down, and its Windows build is one
compile database over 40 files (`src/*.c`, `src/win/*.c`):

```text
CMAKE_EXPORT_COMPILE_COMMANDS=ON with MinGW's gcc,
-std=gnu11 -Iinclude -Isrc -DBUILDING_UV_SHARED=1 -DWIN32_LEAN_AND_MEAN -D_WIN32_WINNT=0x0A00
80 database entries, 40 distinct files (the shared and static libraries each list every file)
```

| Measurement | errors | of which `undefined-name` |
| --- | --- | --- |
| first run | 12 | 11 |
| `db0e3f18` — a `typeof`/`decltype` specifier declares no written type | 11 | 11 |
| `f53b5bc6` — an X-macro enum is one enumeration, not rubble | 3 | 3 |
| `2ae8a432` — a parameter list after another is one more suffix | **0** | 0 |

Final state: **0 errors, 0 warnings, 0 syntax errors, 97 hints** (85 `braces-around-statements`, 12
`else-after-return` — both Hint by the registry's own table, and libuv earns them by writing `if (!handle) return
UV_EINVAL;`). Hover 466/526 (89 %), definition 403/526 (77 %) on a 92-file run of the same project.

### 1. `assign-type-check` on a type the compiler deduces — `db0e3f18`

`src/uv-common.c:576` reported `` `__atomic_exchange_val` is initialised with a value that does not convert to `void` ``.
The name is GCC's: `atomic_exchange_explicit` expands a statement expression whose second line is
`__typeof__ ((void)0, *__atomic_exchange_ptr) v = (VAL);`. The **type reader** spelled that type `void` — the
`BuiltinType` inside the `typeof`'s own argument won the walk — so the declaration was judged against a type nobody
wrote. Probed on the reader itself: `__typeof__ ((void)0, *p)` → `Builtin { spelling: "void" }`, `__typeof__(p)` →
`Builtin { spelling: "__typeof__(p)" }`.

The fix is a question about a **keyword kind**, not a spelling: the lexer already maps `decltype`, `typeof`,
`typeof_unqual`, `__typeof__` and `__typeof` to `DecltypeKeyword`, and `read_specifiers_named` answers "no written type"
for that kind. **`auto` deliberately keeps its spelling** — it is the marker the deduction path reads, and narrowing the
rule until the deduction tests passed is what the rule is a record of.

### 2. `undefined-name` on a name a header declares — `f53b5bc6`

Eleven findings, all about `uv_translate_sys_error` and its neighbours, which `include/uv.h` declares at file scope. The
header's summary held **21 declarations of 563**. The parser was not the cause: `cpp_dump` reported 0 errors in both
dialects. The cause was the enum every C library writes:

```c
typedef enum {
#define XX(code, _) UV_ ## code = UV__ ## code,
  UV_ERRNO_MAP(XX)
#undef XX
  UV_ERRNO_MAX = UV__EOF - 1
} uv_errno_t;
```

Two rules were missing, and both are the X-macro spelling: **an enumerator whose name takes an argument list** (one
enumerator *here*, many after expansion) and **no comma between that invocation and the next enumerator** (the commas
live in the macro's body, not in this file). Summary 21 → **788** declarations; libuv 11 → 3 errors.

### 3. `undefined-name` again, one construct along — `2ae8a432`

`WSAGetLastError` is declared in MinGW's `winsock2.h:1049`, and that header's summary held **206 of 313** declarations:
the walk stopped at line 899,

```c
typedef unsigned __LONG32 (WSAAPI *LPFN_INET_ADDR)(const char *cp);
```

which is `typedef unsigned long (__stdcall *F)(const char *)` before the macros are expanded. Read **unpreprocessed**,
`unsigned __LONG32` is a builtin and a *name*, so the first `(...)` became that name's parameter list and the second was
refused with `expected a parameter list or an initializer` — and the refusal rewound the declaration, so the recovery
ate the declarations after it. A `(` **straight after the declarator's name** still fails (that is `Widget w(g())`, and
the expression grammar must have it); a `(` **after a parameter list** is one more suffix. `winsock2.h`'s summary
206 → **809**; libuv 3 → 0.

### 4. The cache had to be told — `2ae8a432`

A stored summary is keyed by `READING_FINGERPRINT` — a hash of `cpp_parser::VERSION`, the crate version,
`FORMAT_VERSION` and the codec version — none of which move for a **grammar edit in a working tree**, so the first
re-measurement after the fix still read the old 206. That is what `FORMAT_VERSION` is for; it has moved six times since
(`44 → 47`), each with the reason written beside it.

---

## The C corpus before redis

The measurements that came before a named project, and what each one changed: the `neovim` C files (the corpus the C
work started from), Lua 5.5.0's `src/`, and the `test_c` fixtures in `crates/cpp_ls/tests/c_project.rs` — all recorded
where they were taken, in the commits between `557f9553` and `db0e3f18`. They are not repeated here because their
numbers were about individual rules rather than about a program's whole report; the two projects above are the ones a
claim about C usability rests on.
