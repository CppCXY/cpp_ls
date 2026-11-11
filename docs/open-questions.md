# Open questions

Things a report from an editor pointed at, that are **not** fixed — with the measurement that says where they
stand, so the next run starts from evidence rather than from the report again.

## `std::endl` drawn as a variable, i.e. with no semantic token at all

**Not reproduced.** Both halves of the question are now tests, and they answer the opposite of the report.

* **Resolution works through the path the server takes.** In a real MSVC 18 project — `#include <iostream>` and
  `std::cout << std::endl;` — asking for every project file's reading and letting the pump refine the closure answers
  the declaration:

  ```text
  $ cargo test -p cpp_code_analysis --test std_visibility_probe std_endl_resolves -- --ignored
  test result: ok
  ```

  That is `std_endl_resolves_through_the_servers_path`, an **ignored probe**: it needs the machine's own standard
  library and costs about six seconds, so it is run on demand rather than with the suite. The fact is a `function` in
  scope `std`, declared at `__msvc_ostream.hpp:1007` — MSVC 18 moved `endl` one hop past `<ostream>`, which is what
  made this look like a visibility problem.

* **The `NotDeclaredHere` this file used to record was an artefact of `inspect`.** `inspect` asks for **one** file's
  reading; a header two hops out then has no summary, so the visibility walk enters it and finds nothing to report.
  Measured on a three-file fixture (`c.cpp` → `b.h` → `a.h`): `inspect` answers `NotDeclaredHere`, while the same
  chain through the session's path answers the declaration —
  `reported_defects::a_declaration_two_includes_away_is_visible`.

* **The classifier was never the problem.** A namespace-scope function named by a qualified expression is drawn as
  `Function`/`Method`, never as a value — `a_qualified_function_is_drawn_as_a_function`.

So what would produce the report in a project where the name resolves is either a fact the classifier's `claimed`
set already holds (`_`-prefixed predefined macros, module declarations) or a region the cook dropped. Neither is
reproducible here. **The file it happened in is needed**, with the line.

## An inlay hint rendering a deduced type expanded instead of the alias the source wrote

Reported with a screenshot: `auto newText = …` drawn as `basic_string<char, char_traits<char>, allocator<char>>`
rather than `std::string`. Five fixtures of the reported shape all print `std::string`:

* `auto text = make(1);` with `std::string make(int);`
* `auto text = h.name();` for a plain class's member
* …and for a class template's member (`Holder<int>::name()`)
* `auto text = r.get()->format(1);` through `Registry<Formatter>`
* the 39-header fixture's `main.cpp`, which has no `auto` at all

No `inspect inlay` answer in the real `clice` project prints `basic_string<` or `char_traits<` either. The expansion
therefore depends on something the screenshot does not show — a project alias, a macro, or a reading that was not
cooked. **A file and a line number are needed**, not another guess.

## What this run did learn about `inspect` itself

A question asked by `inspect` is not always the question the server answers: it asks for one file's reading, so any
cross-file answer that depends on the **closure** — two hops of `#include`, a module's interface, a header behind a
`#include` a cook never read — can be wrong there while being right in the editor. The two fixtures above are the
minimal reproduction of that difference, and they are tests, so the next person to be misled by `inspect` can see it
in one run rather than in six rounds.

## Diagnostic false positives: how they are found, and what is left

`crates/cpp_code_analysis/tests/diagnostics_sweep_probe.rs` pulls every file's diagnostics in one process and prints
the findings grouped by code, with `file:line` and the message — the sweep a false-positive review needs. Run it with
`--ignored`, and `CPPLS_SWEEP_FILES=<n>` to look at a slice:

```text
cargo test --release -p cpp_code_analysis --test diagnostics_sweep_probe -- --ignored --nocapture
```

Measured on `EmmyLuaCodeStyle`, the first 142 files (its `3rd/asio-1.24.0` included):

```text
before   32 findings:  macro-parentheses 27, missing-include 5
after     1 finding:   missing-include  1
```

The two classes it found, and the fix each got:

* **`macro-parentheses` on macros that expand to a type.** `typename ::asio::detail::async_result_helper<ct, sig>`
  has a `<` and a `>`, and the scanner read the first as a binary operator. The scanner now recognises a **template-id**
  (`template_id_end`: names, qualified names, numbers, nested template-ids, `*`, `&`, `,` and spaces, and it closes),
  and a body headed by `typename` is a type fragment. A type has no precedence to get wrong — a caller's expression
  never reaches it.
* **`missing-include` naming a source file.** Four asio headers were told to include `gtest_unittest.cc`, because a
  template parameter named `T` is also a *type fact* in a test file. A `.cc`, `.c` or `.cpp` is compiled rather than
  included, so its declarations can never be made visible by naming it — the rule now skips a candidate that is a
  source file.

**After the fixes**: the same slice has **no rule findings at all**, and so do the first **380** files of the project
(16 parse errors, which are ignored here). The one finding that survived the first pass —
`associated_executor.hpp:155`'s `is_convertible`, "declared" in an unrelated header — was a **dependent template-id**: the
name is used inside a template argument list of a template declaration, where it may be a dependent base's, an ADL-only
name, or one the instantiation supplies. The rule reads declaration *spellings* and has no scopes for the use, so it now
skips a name used with template arguments inside a template — while a plain type in a template's parameter list is still
reported.

What that does **not** measure is false negatives: a rule that is quiet is not a rule that is right, and this pass only
asked "did any rule say something that is not so". A negative sweep — known-good findings that must fire — is the next
instrument, and the 139 check tests are what stands in for it today.
## `std::endl` drawn as a variable, i.e. with no semantic token at all

**Not reproduced.** Both halves of the question are now tests, and they answer the opposite of the report.

* **Resolution works through the path the server takes.** In a real MSVC 18 project — `#include <iostream>` and
  `std::cout << std::endl;` — asking for every project file's reading and letting the pump refine the closure answers
  the declaration:

  ```text
  $ cargo test -p cpp_code_analysis --test std_visibility_probe std_endl_resolves -- --ignored
  test result: ok
  ```

  That is `std_endl_resolves_through_the_servers_path`, an **ignored probe**: it needs the machine's own standard
  library and costs about six seconds, so it is run on demand rather than with the suite. The fact is a `function` in
  scope `std`, declared at `__msvc_ostream.hpp:1007` — MSVC 18 moved `endl` one hop past `<ostream>`, which is what
  made this look like a visibility problem.

* **The `NotDeclaredHere` this file used to record was an artefact of `inspect`.** `inspect` asks for **one** file's
  reading; a header two hops out then has no summary, so the visibility walk enters it and finds nothing to report.
  Measured on a three-file fixture (`c.cpp` → `b.h` → `a.h`): `inspect` answers `NotDeclaredHere`, while the same
  chain through the session's path answers the declaration —
  `reported_defects::a_declaration_two_includes_away_is_visible`.

* **The classifier was never the problem.** A namespace-scope function named by a qualified expression is drawn as
  `Function`/`Method`, never as a value — `a_qualified_function_is_drawn_as_a_function`.

So what would produce the report in a project where the name resolves is either a fact the classifier's `claimed`
set already holds (`_`-prefixed predefined macros, module declarations) or a region the cook dropped. Neither is
reproducible here. **The file it happened in is needed**, with the line.

## An inlay hint rendering a deduced type expanded instead of the alias the source wrote

Reported with a screenshot: `auto newText = …` drawn as `basic_string<char, char_traits<char>, allocator<char>>`
rather than `std::string`. Five fixtures of the reported shape all print `std::string`:

* `auto text = make(1);` with `std::string make(int);`
* `auto text = h.name();` for a plain class's member
* …and for a class template's member (`Holder<int>::name()`)
* `auto text = r.get()->format(1);` through `Registry<Formatter>`
* the 39-header fixture's `main.cpp`, which has no `auto` at all

No `inspect inlay` answer in the real `clice` project prints `basic_string<` or `char_traits<` either. The expansion
therefore depends on something the screenshot does not show — a project alias, a macro, or a reading that was not
cooked. **A file and a line number are needed**, not another guess.

## What this run did learn about `inspect` itself

A question asked by `inspect` is not always the question the server answers: it asks for one file's reading, so any
cross-file answer that depends on the **closure** — two hops of `#include`, a module's interface, a header behind a
`#include` a cook never read — can be wrong there while being right in the editor. The two fixtures above are the
minimal reproduction of that difference, and they are tests, so the next person to be misled by `inspect` can see it
in one run rather than in six rounds.

## Diagnostic false positives: how they are found, and what is left

`crates/cpp_code_analysis/tests/diagnostics_sweep_probe.rs` pulls every file's diagnostics in one process and prints
the findings grouped by code, with `file:line` and the message — the sweep a false-positive review needs. Run it with
`--ignored`, and `CPPLS_SWEEP_FILES=<n>` to look at a slice:

```text
cargo test --release -p cpp_code_analysis --test diagnostics_sweep_probe -- --ignored --nocapture
```

Measured on `EmmyLuaCodeStyle`, the first 142 files (its `3rd/asio-1.24.0` included):

```text
before   32 findings:  macro-parentheses 27, missing-include 5
after     1 finding:   missing-include  1
```

The two classes it found, and the fix each got:

* **`macro-parentheses` on macros that expand to a type.** `typename ::asio::detail::async_result_helper<ct, sig>`
  has a `<` and a `>`, and the scanner read the first as a binary operator. The scanner now recognises a **template-id**
  (`template_id_end`: names, qualified names, numbers, nested template-ids, `*`, `&`, `,` and spaces, and it closes),
  and a body headed by `typename` is a type fragment. A type has no precedence to get wrong — a caller's expression
  never reaches it.
* **`missing-include` naming a source file.** Four asio headers were told to include `gtest_unittest.cc`, because a
  template parameter named `T` is also a *type fact* in a test file. A `.cc`, `.c` or `.cpp` is compiled rather than
  included, so its declarations can never be made visible by naming it — the rule now skips a candidate that is a
  source file.

**What is left** (1 finding in the slice):

```text
associated_executor.hpp:155  `is_convertible` is declared in buffers_iterator.hpp, which this file does not include
```

`is_convertible` is used inside a dependent template-argument list (`constraint<is_convertible<…>::value>`), and the
declaration the index offers for that spelling is a different entity in a header this file does not reach. The rule
works from declaration *spellings* and cannot resolve a name, so the honest next step is to skip a spelling that
appears **inside a template argument list of a template declaration** — a dependent name resolved at instantiation —
rather than to add a word list.
## Rename: a local is exact, and a file-scope name is too

The failure was one guard in `name_references`:

```rust
// **A file that declares the spelling as a local is not searched.**
let declares_a_local = …any(|fact| fact.local && fact.name == name);
if declares_a_local { found.not_looked_at.push(path); continue; }
```

`a.cpp` declares a local `size`, so the whole file was skipped — including the use of the *global* `size` in another
function — and a rename of `int size = 0;` answered 0 places. Measured in that order: `definitions` found the fact,
`name_references` answered 0 files, `name_level_offsets` answered 0 places. A file-level answer cannot be right for a
per-occurrence question.

**The fix** asks it per occurrence, where it has an answer: the scan no longer skips the file, and `name_level_offsets`
drops an occurrence when the file's own scopes resolve it to a binding a function body owns
(`resolves_to_a_function_local`: `definition_at` plus the scope chain). Measured on the fixture, the global answers
**lines 0 and 2** — its declaration and the use in `two`, and **not** the shadowing local on line 1 — and
`a_rename_stays_inside_the_scope_of_what_it_names` pins both that and the local's own exact answer. On
`EmmyLuaCodeStyle`, `GetState` still answers 6 uses.

## Four reports from one session, with what each one points at

1. **A member in a mem-initialiser is drawn as a function.** `LuaCodeFormat::LuaCodeFormat() : _supportNonStandardSymbol(false)`
   colours `_supportNonStandardSymbol` with `entity.name.function`. It is a *member*, and the initialiser is not a call:
   the declaration's name in `: name(args)` has to be classified as a field even though the shape is call-like.
   Where to look: `semantic_token`'s classification of a name inside a mem-initialiser list, and `sema/scopes`' binding
   for it — the same node a constructor's `Foo::Foo` is.

2. **Find References for `SpellCheck` lists `DiagnosticBuilder::SpellCheck`.** The class narrowing is the only attribution
   a member name has, and it is skipped when `the_class_the_cursor_is_in` returns `None` — which is the case for a
   definition written as `LuaCodeFormat::SpellCheck(...)` if the declarator's class is not read from the text. The
   CodeLens for the same name answers 1 (correct), because it asks the *fact's* qualified name. The narrowing has to stop
   being "the class the cursor is in" and become "the class the declaration belongs to", which the fact already says.

3. **`LuaCodeFormat` sometimes drawn as a function, and an edit restores the type.** Not reproducible by file content:
   it is the *state* of the index. The name is both a class and the constructor `LuaCodeFormat::LuaCodeFormat`, and
   whether the type or the constructor fact answers depends on which fact `kind_of` finds first — `kind_of` requires the
   candidates to agree, so a disagreement must be falling back to one of them. The instrument is to print, at that
   offset, the candidate facts (`every_fact_named("LuaCodeFormat")`) with their `kind`, `scope`, `returns`, and what
   `kind_of_index` answers; the fix is probably in the *fact* for a qualified constructor, if the parser records its
   return type as `LuaCodeFormat` — then it is not a constructor by the "no return type" rule.

4. **Inlay hints and CodeLens disappear on every keystroke and come back seconds later.** Answering *nothing* while the
   index catches up is worse than answering a number that grows: the client is told the answer changed, so it erases what
   it drew. The CodeLens now answers from what is known and lets `workspace/codeLens/refresh` (sent when indexing drains)
   correct it; inlay hints should get the same treatment — never answer "empty because not ready", always answer the
   current best and refresh.

5. **`std::vector<T>::emplace_back()`'s result is unknown**, so `auto &result = results.emplace_back()` has no type and
   `result.Start` is unresolvable. The vector's element type is known at the declaration; the missing piece is
   substituting it into the member function's return type (`reference`) — a template-instantiation question, not a search
   one. `docs/open-questions.md` already records the layer's boundary: it substitutes a *function template*'s
   parameters, not a *class template*'s member types.