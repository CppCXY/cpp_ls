# Where the analysis stands

A snapshot, written to be read before the next round of work. Every number here was measured; every claim that says
"measured" has the command that produced it in the section that makes it. Nothing in this file is a plan — the plan
is the last section, and it is three items long.

---

## 1. Type inference: the main line

`auto` declarations in eight MSVC headers, counted by `types_probe`:

```text
                                    2026-10-05        2026-10-06
  auto declarations written              631               631
  deduced                                130               319
  refused                                501               312
```

`cargo run --release -p cpp_code_analysis --example types_probe -- target/scratch/headers.txt --limit 8 --cook`

For one header, to compare a change against a smaller number:

```text
  <memory>   16 deduced / 31 refused  ->  21 / 26
  <vector>  146 / 83
```

### What is deduced, and how

Each of these is a **rule of the language** rather than an inference, which is why they can be answered without
instantiating anything:

| shape | answer | where |
|---|---|---|
| `new T`, `new T(a)`, `new T[n]`, `new T{a}` | `T*` | `new_expression_allocates` |
| `static_cast<T>(x)` and the other named casts | `T` | the `CastExpr` arm of `type_of_expression` |
| `1`, `1.0f`, `"abc"`, `true`, `nullptr` | `int`, `double`, `const char*`, `bool`, `nullptr_t` | the literal arm |
| `T{…}` | `T` | the `InitListExpr` arm |
| `x.member` | whatever the member's declaration says, found through the **type of `x`** | `member_kinds` |
| `using A = B;` then `A` | `B` | `resolve_aliases` |

### What is refused, and why

`types_probe` prints the reason for every refusal. In `<memory>` they are, in full:

```text
  11  UnknownType("::std::_Get_unwrapped…")    the argument is a template parameter: `_RanIt`, `_InIt`
   2  UnknownType("_ILast - _IFirst")           a binary `-` between two iterators
   1  UnknownType("::std::ranges::next")        a call whose callee the reader cannot place
  10  NotDeclaredHere(…)                        **not a type question at all** — see section 2
```

The first three are genuinely undecidable without **instantiating the enclosing function template**, which is the
next large piece of work: `_Get_unwrapped(_First)` where `_First` is `_RanIt` has no answer until `_RanIt` does.

---

## 2. The macro-condition defect, and where the chase got to

This is the largest finding of the round, and it explains ten of the refusals above.

### The symptom

`<memory>` refuses ten `auto` declarations with `NotDeclaredHere`. Seven of them are
`_Locked_pointer::_Lock_and_load`, written at `memory:4068` as `auto _Rep = _Repptr._Lock_and_load();`.

### The chain, measured one layer at a time

```text
  _Locked_pointer is declared in `atomic:2958`              (the class is there)
  memory:7 includes <atomic> unconditionally                (the edge exists)
  memory:16 puts that include inside `#if _HAS_CXX20`       (the guard)
  vcruntime.h:278  #ifndef _HAS_CXX20
  vcruntime.h:279      #if _HAS_CXX17 && _STL_LANG > 201703L
  vcruntime.h:280          #define _HAS_CXX20 1
  vcruntime.h:282      #else
  vcruntime.h:283          #define _HAS_CXX20 0             <- what the analysis computed
  vcruntime.h:260  #ifdef __cplusplus
  vcruntime.h:262      #define _STL_LANG _MSVC_LANG
  vcruntime.h:264      #define _STL_LANG __cplusplus
  vcruntime.h:267      #define _STL_LANG 0L                  <- what the table answered
```

`_STL_LANG` is `#define`d **once per branch** of the same `#if`, and the table kept all three. A lookup answers
"the last binding at or before this offset", so `0L` — written in the `#else` — shadowed the real value.

Then: `_STL_LANG = 0` → `_HAS_CXX17 0` → `_HAS_CXX20 0` → the guarded `#include <atomic>` judged inactive → the class
invisible → the member not found → the `auto` refused.

### The criterion

```text
  macros_at(vcruntime.h, 7500)  _STL_LANG   must be 202400L      currently 0L
  visible_files(memory)         contains atomic?                 currently false
```

`cargo run --release -p cpp_code_analysis --example seed_probe -- "<MSVC include dir>"`

### What is fixed

`preprocess` records a `#define` **unless a branch enclosing it was decided inactive**:

```text
  cppls-define: _STL_LANG at 7116  live=[true, true, true]   skipped=false   <- kept
  cppls-define: _STL_LANG at 7236  live=[true, true, false]  skipped=true    <- dropped
  cppls-define: _STL_LANG at 7414  live=[true, false]        skipped=true    <- the `0L`
```

`CPPLS_TRACE_DEFINE=_STL_LANG` prints those lines. The rule fires only on an answer the environment is **sure** of
(`#ifdef NAME`, `#ifndef NAME`, and the `#else` of either); `#if X > 2` and `#elif` are left exactly as they were.

### What is **not** fixed, and it is the last hop

`macros_at` does not read the table above. It reads **`summary.macros`** — the macro *facts*, which are generated
straight from the directives by `macro_fact` and do not consult a branch at all. So the dead `#define` is still in
the facts, and the environment built from them still says `0L`.

Two implementations of one question, and only the first was told about the rule. That is the shape this defect has
taken three times now:

```text
  the cook and the visibility walk disagree about one guard
  `MacroTable` and `summary.macros` are two answers to "what is defined here"
  `preprocess` records a `#define` without asking whether its branch is compiled
```

---

## 3. The handlers

| handler | state | the fix it needed |
|---|---|---|
| completion | works | reads the **file's own text**; it used to read the one-line rendering and see a `std::` scope |
| inlay hint | works, stable at 3/3/3 across edits | reads the file's own text; the offsets are the file's |
| hover | works, including `std::endl` | keeps its **two-reading** design: the rendering for names, the file for headers and macros |
| go-to-definition | works | the offset and the answer come from one reading |
| references | **implemented this round** | it only ever answered **macros**; `class Sux` answered 0 |
| semantic tokens | **three layers** | see below |
| signature help | works | — |
| document symbol, folding, selection | works | — |

### The "first ask" defect, fixed in six handlers

Measured with `first_ask.mjs`, which sends the request **on the same tick as the `didOpen`**:

```text
  before:  immediately  NO HOVER      after:  immediately  ```cpp
           250 ms       NO HOVER              250 ms       ```cpp
           1 s          NO HOVER              1 s          ```cpp
           3 s          ```cpp                3 s          ```cpp
```

The cause: `prepare` reads *this* file, and the answer is in a **header** whose facts exist only once the pump has
cooked the include closure. `completion` and `inlay_hint` already waited with `settle(…, 2s)`; `hover`,
`definition`, `references`, `rename`, `signature_help` and `semantic_token` did not, and now do.

### Semantic tokens

Three layers, all measured on a real file:

```text
  what a name is          namespace, type, type parameter, enum member, function, method,
                          variable, parameter, macro                              (nine kinds)
  declaration or use      the `declaration` modifier
  what else is true       `readonly` (const/constexpr), `static`, `deprecated`
```

The first layer needed a fix that mattered more than the other two: a **member access was looked up by its bare
name**, so `v.push_back` and `t.size()` got **no colour at all** — the index answers `Ambiguous` for a spelling the
whole project shares — while `sux.print()` was coloured because `print` happened to be unique. Now the member is
resolved through the **type of the object**, which is what the module's own note had always said to do.

```text
  the same file, before and after:   41 tokens  ->  44 tokens
  the four new ones are `push_back` ×3 and `size` ×1, all `method`
```

---

## 4. Find references: a feature that was missing

`textDocument/references` answered **only about macros**. `locations()` began with `macro_references`, and a name
that is not a macro fell through to an empty list — which the protocol cannot distinguish from a true "nothing uses
this". Measured on a real file: `class Sux` answered **0** with `Sux sux;` two lines below it.

`symbol_references` is the other half:

* the cursor is **resolved first** (`definitions`, the same query go-to-definition uses), so "where is this
  declaration named" is a different question from "where is this word written";
* the candidates are the declaring file and everything that **transitively includes** it, plus every file that
  declares the same **qualified** name;
* occurrences come from the **lexer**, so a name in a comment or a string literal is not a reference — a text scan
  would report three of four places wrongly on the fixture the test uses.

```text
  measured on the user's file:   `class Sux` -> 2 (5:6, 13:4)      `sux` -> 2 (13:8, 15:4)
```

---

## 5. Probes

A probe that can see one layer is worth more than a guess about six. These were written this round, and every
diagnosis above came from one of them:

| probe | the question it answers |
|---|---|
| `types_probe` | what `auto` declares, and the reason for each refusal |
| `fact_scopes` | how a file's facts are distributed — **34070 facts over 93 headers in 10 s**, against 229 s for eight headers through `types_probe` |
| `dump_tree` | what the grammar made of this text, nodes **and** tokens |
| `lookup_probe` | does a name resolve, which layer said no, and what the facts look like |
| `member_probe_at` | what class the member path derived, and what the object's type was |
| `closure_probe` | the forward closure against the reverse edges, per file |
| `seed_probe` | what the macro environment says at a given offset |
| `first_ask.mjs` | what a query answers on the **first** ask, against what it answers once settled |

Two of them lied before they were fixed, and both are worth remembering:

* `dump_tree` **kept filtering below a matching node**, so `NewExpr` printed its own one token and stopped — the
  type child, which was the whole question, was filtered out as "not a NewExpr". A focus chooses where to start,
  not what to keep.
* `member_probe_at` counted lines as `\n` in a file whose lines end `\r\n`, so it landed four thousand bytes early
  and reported "not a member access at that offset".

And one trap that is not a probe: **`target/scratch/.cppls` held 1053 cached summaries**, so several rounds of
measurement were reading old data. Clearing it is the first thing to try when a fix "does not move the number".

---

## 6. Tests and discipline

```text
  lib                594 passed / 0 failed      **green**, and it has not been green in this file's lifetime
  analysis suites    all 25 green, tests/translation_unit.rs 14 / 0 included
  end to end         22–23 / 0–1, four ignored   the single failure is the flaky one recorded at the end of this section
  semantic           30 / 0
  types              26 / 0
  workspace          zero warnings
```

### Step 4 of the latency plan was measured and dropped

The measurement it was waiting on is now in the repository
(`cargo run --release -p cpp_code_analysis --example cook_value -- <entry.cpp>`), over a translation unit including
`<string>`, `<vector>` and `<map>`, per file, as a set difference of declaration names:

```text
  file       raw   cooked   only cooked   only raw
  xstring    163      511            353          5
  vector     118      332            262         48
  main.cpp    12       12              0          0   ← the file being edited
```

**The rendering adds nothing to the file being edited and hundreds of declarations to the headers** — so the plan's
premise was right and its conclusion was not. The rendering of the edited file is not produced by an eager cook:
`want_the_closure_cooked` has never marked the root, and `cpp_ls/src/context/analysis_state.rs:167-174` says in its
own words that a file a request names gets its reading, every request. Removing the eager cook would therefore move
the work onto the waiting request rather than remove it, and the alternative — making requests stop asking — is a
change to what a client is promised (`isIncomplete`) and puts the *diagnostics* difference at risk, which is the one
thing the probe above explicitly does not measure. `docs/incremental-edits.md` §8.7 has the argument.

### Two cache-invalidation bugs, and both were found by a test that already existed

Five failures were carried as "not ours" for two rounds. Three of them are now fixed, and neither fix was about the
cook — which is what everyone, this file included, had assumed they were about.

```text
  1.  a timeline walked while its closure was still incomplete was written to the disk cache
      the key records the files the walk *entered*, so a one-frame timeline of a two-file closure is
      indistinguishable from a complete one — and every later session was served it, with an environment missing
      whatever that file defined. `one::Widget` came back `NotDeclaredHere` while a fresh walk of the same inputs
      produced both frames. See `docs/incremental-edits.md` §8.5.

  2.  an edit that changed a file's **macros** dropped its dependents' cooked readings and left their **timelines**
      the disk entry is keyed on the content of its closure, so an edit refuses it on its own — and the in-memory
      table in front of it is checked against nothing at all. The dependents were re-cooked **out of timelines that
      still said the old macro**, so `two::Widget` was `NotDeclaredHere` with every step of the invalidation
      working. `Session::invalidate_dependents` now drops them beside the readings.

  both: the walk was never wrong. What was wrong was the *validity* of a cached answer about a program that had
        moved underneath it — which is why neither showed up as a parse error, a panic, or a wrong-looking tree.
```

### And four failures were tests asserting a contract the code had deliberately changed

Re-basing them is the one kind of test edit that has to be argued rather than done, so here is the argument, per
group. Each is a case where the code's own comment records a *later, measured* decision and the test encodes the
rule from before it:

```text
  the_unit_cooks_as_one_stream_in_include_order        `stitched.text.contains("= 4 ;")`
  a_definition_says_which_file_it_was_written_in       `rendered.text.find(" 4 ")` ×2
      a claim about the **renderer's whitespace**. The stream spells `int main_use = 4;`, so all three were red
      while the expansion each is named after was right there — in the first case the panic message printed the
      expanded value itself. Re-based on the digit, which is unambiguous in these fixtures, and *strengthened*:
      the stitched-unit test now also asserts that neither macro's name survived into the program, which is what
      an unexpanded invocation would look like.

  a_header_the_user_never_opened_is_cooked_when_a_request_names_it
  a_unit_read_reads_the_whole_program_once
      "a transitive include is not cooked for free" / "the per-file policy stops at the direct ones" — against
      `COOK_ONE_LEVEL_FURTHER = 64`, whose comment says the level under a direct include is reached **on purpose**
      because MSVC's `<string>` is a thin wrapper over `<xstring>` and `std::string` lives in the second level.
      Re-based on the bound that is in force: a fourth file (`deep.h`) was added to each fixture so the tests now
      pin **level 2 cooked, level 3 not** — one more level distinguished than the version that passed before, and
      the unit-read test now asserts that the unit read reaches the level the per-file bound leaves out.
```

Both re-basings are checkable by eye against the code they are about, which is the standard: no test was made weaker,
and the two bound tests now say what the constant does rather than what an older constant did.

### One of the five was a cache-soundness bug, and it is fixed

`a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file` was the first of the five to fall, and
what it was about is worth recording because it was **not** about the cook:

```text
  a translation unit walked while its closure was still incomplete was written to the disk cache
  the key records the files the walk *entered*, so a one-frame timeline of a two-file closure is
  indistinguishable from a complete one — and every later session was served it
  the environment it answered was then missing `#define BEGIN_NS namespace one {`
  so the file was read as if the macro were never expanded and every declaration in it landed at file scope
```

Decisive experiment, and the reason the diagnosis is not a guess: with the session that writes the short entry absent,
`definition("one::Widget")` answers `Known::Yes(… scope: Some("one") …)`; with it present, `NotDeclaredHere`. The walk
itself was never wrong — a fresh walk over the same inputs yielded both frames throughout.

The fix is `HeldUnit`: a timeline built while a file its closure needs is **still queued** is *provisional* — used for
that run, never written to the cache, and re-walked once the queue drains. The distinction between "outside the
analysis" (permanent, and the reading `TranslationUnit::walk` documents) and "not yet read" (temporary) is invisible
to the walk and visible to the session, because the queue is the session's. See `docs/incremental-edits.md` §8.5.

### The numbers above were believed for a round in which this file was never compiled

The row used to read `587 passed / 4 failed`. It was wrong twice over, and the way it was wrong is worth more than
the correction:

```text
  1.  the lib test target did not compile — 4 errors
      analysis_state.rs      three calls to `update`/`update_session` without the `label` argument the signature
                             gained
      semantic_token/mod.rs  a `Name` literal without the `modifiers` field
      session.rs             four assertions calling `units.is_empty()` on the `Mutex` that had replaced the field

  2.  once it compiled, it did not finish — `Session::index_everything` never returned
      it looped on `pending_work()` (the index queue **plus the cooking backlog**) and only ever called `advance`,
      which *marks* files for cooking and drains nothing. The marking is idempotent, so the backlog sat at a fixed
      size for ever. Every one of the ~40 `session::tests` hung for sixty seconds and then for ever; the suite was
      killed rather than run, which is what "the tests are slow" turned out to mean.
```

`cargo test -p cpp_code_analysis --lib` now finishes in **0.43 s**.

### The five failures, and what they turned out to be

Attributed by experiment rather than by reading: the suite was run with the translation-unit root key reverted to
the whole-text rule, and again with the cooking drain removed. The five fail in **every** configuration, including
one in which the whole of the work they are supposed to be about is absent — so they are not a consequence of any of
it. That was right, and it was also the reason they sat here: "not caused by this round" is not a diagnosis, and the
list kept its own guesses (`NotDeclaredHere("HWND__")`) as if they were causes.

```text
  editing_a_header_invalidates_the_readings_that_depend_on_it        FIXED — §"two cache-invalidation bugs" 2
  editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it   FIXED — the same one
  a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file   FIXED — the same section, 1

  a_header_the_user_never_opened_is_cooked_when_a_request_names_it   RE-BASED — the test was asserting the policy
  a_unit_read_reads_the_whole_program_once                           from before `COOK_ONE_LEVEL_FURTHER`
```

**Three of the five were one bug each, and the two "policy" failures were the code being right.** The lesson is the
one this file's rule 2 already states in the other direction: a list of failures carried forward with a plausible
cause attached is a list nobody is looking at properly. What broke it open was writing a *new* test whose fixture had
to make the environment matter — at which point the same symptom appeared with a cause that could be measured.

What is left of that list, after the two cache fixes and the four re-basings above:

```text
  cpp_ls lib         50 passed / 2 failed
      handlers::references::tests::a_reference_list_is_refused_while_the_index_has_work
      handlers::rename::tests::a_rename_is_refused_while_the_index_has_work
      Both are tests of the *refusal* path — "the index still has work" is their premise — and one panics with
      "the index is complete now", i.e. the premise is not reachable in the fixture. Confirmed failing identically
      in a clean `git worktree` at `HEAD` before this round; not attributed further.
```

The `cpp_code_analysis` crate has no failures left at all, which is worth saying plainly because this file has
carried a non-zero row in every revision of it.

### And one mechanism was correct, covered by a green test, and never reached

`docs/incremental-edits.md` §5.2's steps 1–3 put the macro environment into `Session::index_one`'s **`None`** arm —
the arm that runs when a wave did not prepare the file. `Session::advance` prepares a wave whenever it has more than
one step to take, and the pump asks for sixteen, so on every path a running server takes that arm was skipped:

```text
  Step::with_the_environment     never true on a real edit
  the test that covers it        green — because it called `advance(1)`, which takes no wave
```

Found by asking **where the flag is ever set** rather than whether the code that sets it is right, which is the one
question a passing test cannot answer about itself. The fix is a condition in `Session::advance`; the trap in it is
recorded in §8.7 of the other document, and it is the same shape as everything else this round: the first version
threw away the wave's *answer* instead of keeping the file out of the wave, and the wave had already written that
answer to the disk cache — so the step came back `Reused` and the environment went unused anyway.


### The rest of this round's findings, kept as a list because they are all the same shape

```text
  a `MutexGuard` temporary in an `if let` scrutinee lives until the whole statement ends
      so taking the same lock inside the body is a **re-entrant lock on one thread** — a deadlock, not an error the
      type can report. Written while fixing the cache bug above; it hung the one test whose fixture reaches the
      drop path, and it took a per-step trace to see that the loop was not in the indexing loop at all.

  `macro_readings` is not evidence that an environment was used
      it records the bodies the **plain shape reader cannot settle**, and a simple
      `#define BEGIN_NS namespace one {` is settled with no environment at all — so a test using it as a witness
      would pass while the mechanism never ran.

  a unit's frames are keyed by the spelling the **walk** used
      asking `environment_of` with the session's spelling answers `None`, silently: the feature is absent, the code
      compiles, nothing panics. Neither of these was found by reading; both were found by asserting the *effect*.
```


### And one that is not a failure but a coin

`crates/cpp_ls/tests/handshake.rs`'s `a_completion_sees_a_change_to_an_included_header` **passes about half the
time** — four consecutive runs of that binary: ok, failed, ok, ok. Its own message says why it is written that way
("asked once, right after the change, with no retry to hide a stale answer"), so the flake *is* the finding: whether
a completion asked in the instant after a header edit sees the edit depends on something that is not ordered. What
that something is has **not** been established, and the method that established the five above applies unchanged —
run the suite in a `git worktree` at `HEAD` and see whether it is a coin there too.

### The rules this round was held to, in the order they were learned

1. **Measure before changing.** Six of the eight layers in section 2 were guessed wrong. Each one was found in a
   single step *after* a probe could see it, and cost hours before.
2. **A criterion that bites may still bite the wrong shape.** Two tests written this round passed on fixtures that
   did not reproduce the real defect — a macro defined in the same file, a type with a modifier. A test proves what
   it tests and nothing more.
3. **"It moved" may mean the cache moved.** See section 5.
4. **A green suite you cannot run is not a green suite.** See above: the row at the top of this section was copied
   forward for a round in which the target did not compile and then did not terminate. Run it, or do not quote it.
5. **Never set `sandbox_permissions`**; approval prompts are disabled in this session.

---

## 7. What is next, in order

1. **`macro_fact` must not record a `#define` from a branch that is not compiled.** The rule already exists in
   `preprocess`; the facts need the same one. Criterion: `macros_at(vcruntime.h, 7500)`'s `_STL_LANG` becomes
   `202400L`, and `visible_files(memory)` then contains `atomic`. Ten `auto` refusals should turn over with it.
2. **Nested generic inference**: re-evaluate an `auto` inside a function template's body against the **enclosing
   template's substitution table**. The two families to move are `_Get_unwrapped*` (11 in `<memory>`) and
   `_VbIt::_Myoff`. This is the largest single remaining source of refusals.
3. **`_ILast - _IFirst`**: a binary `-` between two iterators, which for a difference type is `_Iter_diff_t<It>`.
