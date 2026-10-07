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
  lib                588 passed / 5 failed      measured, after the round that made the target runnable at all
  analysis suites    22 / 22
  end to end         23 / 0, four ignored
  semantic           30 / 0
  types              26 / 0
  workspace          zero warnings
```

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

### The five failures, and what they are

Attributed by experiment rather than by reading: the suite was run with the translation-unit root key reverted to
the whole-text rule, and again with the cooking drain removed. The five fail in **every** configuration, including
one in which the whole of the work they are supposed to be about is absent — so they are not a consequence of any of
it.

```text
  a_declaration_only_a_macro_makes_is_found_once_the_session_has_cooked_the_file
  editing_a_header_invalidates_the_readings_that_depend_on_it
  editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it
      all three: the cooked reading that should carry a macro-written declaration does not
                (`NotDeclaredHere("HWND__")`, `NotDeclaredHere("two::Widget")`)

  a_header_the_user_never_opened_is_cooked_when_a_request_names_it
  a_unit_read_reads_the_whole_program_once
      both: a file *is* cooked where the test says it must not be — and the test's stated rule
            ("a transitive include is not cooked for free") contradicts `want_the_closure_cooked`, which
            deliberately reaches `COOK_ONE_LEVEL_FURTHER = 64` below the direct includes, for the measured reason
            that `<string>`'s `std::string` lives in `<xstring>`. One of the two is wrong and it is not obvious
            which; that is a review, not a rewrite.
```

Three of the five are about the same thing — a cooked reading that does not carry what it should — and that is the
half of `docs/incremental-edits.md` §5.2 that removes the rendering from the keystroke path. They should be settled
*with* that work rather than before it.

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
