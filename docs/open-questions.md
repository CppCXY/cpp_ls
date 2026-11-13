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

## An inlay hint rendering a deduced type expanded instead of the alias the source wrote — **fixed**

Reported with a screenshot, and five fixtures of the reported shape all printed `std::string`, so the entry ended
with "a file and a line number are needed, not another guess". The line arrived: `LuaCodeFormat.cpp:131`,
`auto formattedText = f.GetFormatResult(t);` where `RangeFormatBuilder::GetFormatResult` returns `std::string`.

Reproduced over stdio (`cpp_ls/tests/real_project_hover_probe.rs`): **both** the hover and the inlay hint said
`basic_string<char, char_traits<char>, allocator<char>>` — and kept saying it for ninety seconds. A session built by
the crate's own probe answered `std::string` about the same offset, which is why five fixtures missed it.

The difference is the **state of the index**, and the step is `finish_a_nested_name`. It exists to make a *member's*
spelling usable — `pointer` becomes `_Val*` — and it treated `std::string` as "the member `string` of the class `std`",
looked that member up, and replaced the spelling with the member's *type*: the alias's target. It only does that once
the index holds the `using string = basic_string<…>` declaration, which is why an early request answered correctly by
accident and a warm one did not.

The rule now: **a name qualified by a namespace is not a member of a class, and is left as written**
(`names_a_namespace`, used by `finish_a_nested_name` and by `a_spelling_through_the_aliases_in`). Measured after the
change, over the same stdio probe:

```text
hover  at 130:12   std::string formattedText
inlay  at 130:22   ": std::string"
```

`tests/types.rs`' `a_deduced_type_keeps_the_alias_the_source_wrote` is the unit-level statement; it failed with the
expanded spelling before the guard and passes after.

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

## An inlay hint rendering a deduced type expanded instead of the alias the source wrote — **fixed**

Reported with a screenshot, and five fixtures of the reported shape all printed `std::string`, so the entry ended
with "a file and a line number are needed, not another guess". The line arrived: `LuaCodeFormat.cpp:131`,
`auto formattedText = f.GetFormatResult(t);` where `RangeFormatBuilder::GetFormatResult` returns `std::string`.

Reproduced over stdio (`cpp_ls/tests/real_project_hover_probe.rs`): **both** the hover and the inlay hint said
`basic_string<char, char_traits<char>, allocator<char>>` — and kept saying it for ninety seconds. A session built by
the crate's own probe answered `std::string` about the same offset, which is why five fixtures missed it.

The difference is the **state of the index**, and the step is `finish_a_nested_name`. It exists to make a *member's*
spelling usable — `pointer` becomes `_Val*` — and it treated `std::string` as "the member `string` of the class `std`",
looked that member up, and replaced the spelling with the member's *type*: the alias's target. It only does that once
the index holds the `using string = basic_string<…>` declaration, which is why an early request answered correctly by
accident and a warm one did not.

The rule now: **a name qualified by a namespace is not a member of a class, and is left as written**
(`names_a_namespace`, used by `finish_a_nested_name` and by `a_spelling_through_the_aliases_in`). Measured after the
change, over the same stdio probe:

```text
hover  at 130:12   std::string formattedText
inlay  at 130:22   ": std::string"
```

`tests/types.rs`' `a_deduced_type_keeps_the_alias_the_source_wrote` is the unit-level statement; it failed with the
expanded spelling before the guard and passes after.

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
   `result.Start` is unresolvable. **Corrected by measurement** (see the two sections below): the substitution the
   paragraph above guesses at is *not* what failed. In a fixture the same call answers `Item&` in all five spellings
   tried — alias, alias of alias, written-out `_Ty&`, template member, plain member. What actually answered line 304
   was a **range-for leak**, and what answers it now is the *other* question, `std::vector` not being resolvable.

## Line 304 was a range-for leak, and it is fixed

`LuaCodeFormat.cpp:304` — `auto &result = results.emplace_back();` — is the **first statement of
`for (auto &diagnostic: diagnostics)`**. `range_for_element_type` decided "is this declaration the loop's variable?"
by asking whether a `RangeForStat` is among its ancestors, which is true of *every* declaration in the body, and it
answered **before** the ordinary deduction ran. So `result` was typed as the loop's **range**, and the initializer was
never looked at. The type trace is one line long and says so:

```text
[type_of] depth  0  "result"
[type_of] depth  2  "diagnostics"     ← the range-for's range, not `results.emplace_back()`
```

The proof, in `tests/emplace_probe.rs` (`an_auto_declaration_inside_a_range_for`):

```text
inside a range-for, `result`        -> UnknownType("std::vector<Other>&")   ← before
beside a range-for, `result`        -> Item&
```

Two fixes, both with a regression test in `tests/types.rs`:

* `range_for_element_type` now answers only when the declaration is in the loop's **head** — before its `:`
  (`is_in_the_head_of`). A body declaration falls through to `deduction_inputs`, which is its own initializer.
  `an_auto_declaration_in_a_range_for_body_is_deduced_from_its_own_initializer` asserts `Item&`.
* `range_element_of` now unwraps `Type::Reference` and `Type::Qualified` first. `for (auto &x : parameter)` over a
  `const std::vector<T>&` — the ordinary spelling — typed the loop variable as the container.
  `a_range_written_as_a_reference_still_binds_its_element` asserts `int`.

After both, the same line in the real project answers `NotDeclaredHere("std::vector::emplace_back")` instead of a
wrong type: the leak is gone, and what is left is the honest "this class is not in the index", which is the section
below.

**One stale test was red in the tree, and the fix is the rule rather than the code.**
`generics.rs::a_nested_instantiation_has_its_members_and_its_argument` asserted that `auto row = grid.front();` is a
`std::vector<int>&`. It was written on `2026-10-18` (`4ee66578`), and `read_the_way_auto_does` — the rule that
`auto x = f();` strips the top-level reference and `const`, which is what the language does — landed on `2026-11-01`
(`bb407dd3`), so the test was red from then on and nothing had run it since. It now asserts both spellings: `auto row`
→ `std::vector<int>`, `auto &bound` → `std::vector<int>&`. The subject it was written for — the outer argument
substituted and the inner one kept — is asserted either way.

## The field kind, and the clients that cannot draw it

`1c00d3ad` ("a variable bound in a class scope is a property") made a class's field `NameKind::Property`, and updated
the handler's own tests. It did not update `tests/semantic.rs`, and it did not change what a client is **sent** when
its advertised `tokenTypes` have no `property` in them — so five tests were red from that day:

```text
cpp_code_analysis   tests/semantic.rs        4 failed   expected Variable, got Property
cpp_ls              handshake.rs:3518        1 failed   the field `size`, over the wire
```

The first four are stale expectations: every one of them is about a class member (`Widget::size`) and `Property` is
the answer the analysis now gives on purpose. They assert `Property` with the reason written beside them.

The fifth was **not** stale — it is the end-to-end statement that a client receives a token it can draw. The client in
that test advertises `namespace, type, typeParameter, enumMember, function, method, variable, parameter, macro` — no
`property` — and `Builder::push_range` drops a kind the client cannot draw *before* claiming the span, so the field
was sent as `property` and drawn as nothing. The fix is the fallback the file already documents for
`variableTemplate` and for `typename`: a field a client cannot draw as a `property` is still a variable, and
`property` is standard but not universal. A client that lists it still gets it.
`a_field_a_client_cannot_draw_as_a_property_is_drawn_as_a_variable` pins both halves.

## An enumerator from a header had no colour at its use — **fixed**

Reported with a screenshot: `case TK_ELSEIF:` in `SpaceAnalyzer.cpp` was drawn as `variable.other.unknown` — no
semantic token at all — and the reporter added that `enum class` names are missing at their uses too. The enumerator
is declared in `LuaTokenTypeDetail.h`, which that file includes directly.

Two rules were wrong, and both are about scope:

* `ProjectIndex::kind_of` keeps a candidate whose `scope` is set only when the qualified name is what was asked. That
  is right for a constructor (`TextRange::TextRange` is not another meaning of `TextRange`) and wrong for an
  enumerator: an unscoped enumeration's enumerators are visible in the enclosing scope, so the fact's scope is the
  *enumeration's* while the spelling asked is the bare one. Measured: `kind_of("TK_ELSEIF", SpaceAnalyzer.cpp)` answered
  **`Unknown(ConditionalCompilation)`**, and the use got zero classifications.
* `kind_of_index` matched "a `Variable` with a scope is a property" before it asked `enumerator`, and an enumerator
  always has a scope, so that arm always won.

After both, over the same probe: `kind_of` → `IndexedKind { kind: Variable, scope: Some("LuaTokenTypeDetail"),
enumerator: true }`, and the use has **one classification, `EnumMember`**. `tests/semantic.rs`'
`an_enumerator_from_a_header_is_an_enum_member_at_its_use` pins the unscoped and the `enum class` spelling, on the
**plain** view — the one the colouring pass is given, where a cross-file answer is the index's.

## `size()` of a vector answers `_Alty_traits::size_type` — **fixed**

Reported with a screenshot: `auto pos = _events.size();` in `LuaParser.cpp:51` drawn as `_Alty_traits::size_type`.
Fixed after five measured steps, each one a defect of its own:

1. **a failed qualified name now asks for the file that would answer it** (`ProjectIndex::ask_for_refinement`,
   drained by `Session::advance`) — `<xmemory>` was never refined, so `std::allocator_traits` did not exist;
2. **a class template's base clause is the *definition's***, not its forward declaration's;
3. **a member's defaulted template argument is substituted** (`allocator_traits<A>` → `allocator_traits<allocator<int>>`);
4. **a specialization's slot matches a qualified argument** by head and recursively (`allocator<_Ty>` against
   `std::allocator<MarkEvent>`), and a trait's bool is the base its matching specialization inherits, which lets
   `conditional_t` pick one branch;
5. **a class definition whose head writes a qualified name is a declaration again** — `qualified_specifier_name_node`
   took `A<T>::From` *inside the argument list* of `struct B<A<T>, void_t<typename A<T>::From>>` for the declaration's
   own name, so the whole partial specialization was read as an out-of-line definition of a foreign class: no binding,
   no class scope, no members, **no fact**. That is what kept `_Is_default_allocator<allocator<_Ty>, …>` out of
   `<xmemory>`. `FORMAT_VERSION` is now 18, because every cache built by the old reader holds the missing fact.

Measured over stdio against the real binary, on `EmmyLuaCodeStyle`:

```text
[hover-probe] `pos` at 50:10     size_t pos                          (was `_Alty_traits::size_type`)
[hover-probe] `formattedText`    std::string formattedText
[hover-probe] inlay hints        ": std::string"
```

`tests/types.rs::a_partial_specialization_is_a_declaration_with_a_pattern` is the regression for (5), and
`a_conditional_base_picks_the_branch_its_trait_selects` for (3) and (4) together. Both were ignored reproducers while
the chain was open; both are ordinary passing tests now.

**The survey that says how far the fix reaches.** `real_project_probe.rs::how_many_deduced_types_are_internal_spellings`
asks every `auto` declaration in the project's own sources for its deduced type and counts the answers that are still
a library-internal spelling (`_Alty_traits::size_type`, `_Ty`, `_Mybase::iterator` — the signature of a chain that
stopped one step short):

```text
[generics] 0 of 885 deduced type(s) are internal spellings
```

The one case that was internal before this work — `LuaParser.cpp:51`'s `pos`, which answered `_Alty_traits::size_type`
— is what the five steps above fixed, so this number is the reach of the fix rather than a coincidence. It is the
probe to re-run after any change to the alias walk, the shape table, or the cache version.

**One disagreement left, and it is about the harness rather than the answer.** `real_project_probe`'s
`what_a_size_call_returns` builds its own session, asks, pumps once and asks again — and on that flow **both** answers
are still `_Alty_traits::size_type`, while the server over stdio answers `size_t pos`. The difference is how many
drains the refinement passes get: the query's `ask_for_refinement` is served by the next idle round, and the class
behind `size_type` (`std::allocator_traits`) only answers correctly once `<xmemory>`'s **and** its own dependants have
been refined. The product surface is the server, and that is the measurement the objective named; the crate-level
probe needs to keep pumping until its answer settles rather than for exactly one round. Recorded here rather than
fixed, because a probe that waits is a one-line change and a probe that assumes the ordering is what misled this
investigation twice already.

## Why `std::vector` itself is missing, and the wrong lever I pulled at it

Reported after the above, with a screenshot: `auto pos = _events.size();` in `LuaParser.cpp:51` drawn as
`_Alty_traits::size_type`. Reproduced over stdio with the same client (`real_project_hover_probe.rs`): the hover says
`_Alty_traits::size_type pos`.

The fixture that mirrors the shape (`tests/types.rs`, `a_containers_size_called_on_an_object_is_its_size_type`) answers
`unsigned long long`, so the fixture is not the shape the library has. What the trace of the real case shows
(`CPPLS_TRACE_INFERENCE=1`):

```text
alias step: "_Alty_traits::size_type" in std::vector<MarkEvent>   the return type
alias step: "_Alty_traits"            in std::vector<MarkEvent>
alias step: "allocator_traits<_Alty>" in std::vector<MarkEvent>   ← the target, with `_Alty` still unsubstituted
—                                                                 and the walk stops here
```

`_Alty_traits` is declared **in `vector` itself**, not in a base (an earlier version of this note said a base, which
was wrong — `<vector>` lines 465-480):

```cpp
using _Alty        = _Rebind_alloc_t<_Alloc, _Ty>;
using _Alty_traits = allocator_traits<_Alty>;
using size_type    = typename _Alty_traits::size_type;
```

So the missing step is the substitution of `_Alty` **inside** `allocator_traits<_Alty>`: the walk resolves the
qualifier `_Alty_traits` to its target and then treats `allocator_traits<_Alty>` as a spelling to look up, where
`_Alty` is not a member of `allocator_traits` and nothing substitutes it. `resolve_aliases` is the reader that knows
how such an argument is written (it is what `finish_a_nested_name` uses for a base name), and it is not applied here.

The fixture in `tests/types.rs` (`a_containers_size_called_on_an_object_is_its_size_type`) mirrors that chain,
`_Rebind_alloc_t` in an **included header** — the shape the standard library has — and it **passes**. The trace of
the fixture and the trace of the real project differ at exactly one line:

```text
fixture   member of "allocator_traits<_Alty>": 1 listed, 0 unlisted, looking for "size_type"
          → the member resolved, then `_Alty` → `_Rebind_alloc_t<_Alloc, int>` → `unsigned long long`

real      member of "allocator_traits<_Alty>": 0 listed, 1 unlisted, looking for "size_type"
          → no member resolved, so the walk answered the spelling it had
```

The first reason was `<xmemory>` never being refined: `<vector>` was (`std::vector` has three declarations) while
`std::allocator_traits` answered `NotDeclaredHere`. **That is now fixed** — `ProjectIndex::ask_for_refinement` is the
ask a failing qualified lookup leaves, `Session::advance` drains it into the refinement queue, and after one pump:

```text
`std::allocator_traits`: 2 declaration(s)      (was NotDeclaredHere)
`<xmemory>` refined: true                      (was false)
```

The remaining gap is one layer further in: `allocator_traits` now resolves but its member list is still
`0 listed, 1 unlisted` — the **unlisted** entry is the base that declares `size_type`, so the walk has a base it
cannot resolve rather than a class it cannot find. That is the next measurement: which base, and why.

**Round 2** answered that. The base clause was not read at all at first, and that was a second defect:
`the_base_clauses_of` took the **first** Type fact for the name, which for a class template is the **forward
declaration** — `xmemory:554` declares `allocator_traits`, `755` defines it with
`: conditional_t<_Is_default_allocator<_Alloc>::value, _Default_allocator_traits<_Alloc>,
_Normal_allocator_traits<_Alloc>> {}`. The forward declaration has no bases, so the walk saw none. The preference is
now the fact's own `definition` flag — a definition is what the language calls the class.

After that, the gap is exactly one spelling:

```text
unlisted: `conditional_t<_Is_default_allocator<_Alloc>::value, _Default_allocator_traits<_Alloc>,
                         _Normal_allocator_traits<_Alloc>>`
  — NotDeclaredHere("conditional<_Test,_Ty1,_Ty2>::type")
```

The alias target of `conditional_t` was followed **without substituting its arguments**: the library's own parameter
names are still in the spelling (`_Test,_Ty1,_Ty2`) where the written arguments belong. That is the next fix, and it
is the shape this whole entry is about — an alias template whose target must be substituted before it is a class.

**Round 3: the obvious fix is a trap, and the reproducer is now a test.**
`an_alias_template_used_as_a_base_substitutes_its_arguments` (`tests/types.rs`) is a local fixture of the same
shape — an alias template whose target is `typename pick<X, Y>::type`, used as a base — and it fails today with
`unlisted: ["pick_t<chosen<int>, other<int>>"]`, so the defect is reproducible without the standard library.

Calling `a_spelling_through_the_aliases` from the base walk to resolve it was tried and **overflowed the stack**:
that walk asks member questions, a member question builds a member list, and a member list walks bases. The fix
therefore has to be static — the alias fact's own parameters and target, read from the index, then the target member's
fact, substituted, bounded — and for `conditional_t` it has to stay sound where the condition cannot be evaluated:
the members **both** branches agree on, which is the rule `the_return_the_overloads_agree_on` already applies. The
test is `#[ignore]`d rather than deleted, and says all of that where the next reader will see it.

**Round 4: the static resolver landed, and the member list went `0` → `44`.** `the_classes_a_base_clause_names` reads
facts only — an alias's `parameters` and `type_of`, then a class's member fact, substituted, bounded by
`MAX_ALIAS_HOPS` — and returns **two** classes when the base is `conditional<Cond, A, B>::type`, because the condition
is a library trait over a template parameter and choosing the primary template's `_Ty1` would be a guess. Both are
walked and `mark_ambiguity` marks what they disagree about. `an_alias_template_used_as_a_base_substitutes_its_arguments`
is green and no longer ignored, and on `EmmyLuaCodeStyle`:

```text
members_of("std::allocator_traits"): 44 listed, 0 unlisted      (was 0 listed)
```

The next blocker is exact, and it is **one feature**:

```text
size_type: type Some("size_t")                                  ambiguous true   std::_Default_allocator_traits<_Alloc>
size_type: type Some("typename _Get_size_type<_Alloc>::type")   ambiguous true   std::_Normal_allocator_traits<_Alloc>
```

`allocator_traits<_Alloc> : conditional_t<_Is_default_allocator<_Alloc>::value, …>` — deciding the branch means
evaluating `_Is_default_allocator<std::allocator<MarkEvent>>`, which is decided by **matching a partial
specialization**: `template <class _Ty> struct _Is_default_allocator<allocator<_Ty>> : true_type {}` against the
principal template's `false_type`. Selecting a partial specialization is the next piece of the generics work; until it
exists, `size_type` is honestly ambiguous and `auto pos = _events.size();` keeps answering `_Alty_traits::size_type`.

**Round 5 measured the shape of that piece, with a fixture that needs no standard library**:
`tests/types.rs::a_conditional_base_picks_the_branch_its_trait_selects` (ignored, and it says what the answer must
be). It answers `allocator_traits<A>::size_type` today, and the spelling names **three** gaps in order:

1. **the defaulted template argument is not applied at the use** — `std::vector<int>` writes one argument, `A`
   defaults to `allocator<T>`, and `A` is still standing where `allocator<int>` belongs. `substituted_spelling_with_defaults`
   exists for member types; the class level here passes only the written arguments.
2. **`a_pattern_matches` compares text** — the slot `allocator<_Ty>` against the argument `std::allocator<MarkEvent>`:
   the same class, spelled differently, so it does not match. Matching a template-id slot needs head names and a
   recursive comparison, not string equality.
3. **`conditional<true, A, B>::type` must select `A`** once the two above are decided — `conditional` has partial
   specializations for `true` and `false`, which is the same selection mechanism as (2).

**Round 6 fixed (1)**: `substituted_members` cut the parameter list and the written arguments to the shorter side, so
a defaulted parameter was left standing rather than filled in. It now substitutes with the declaration's
**defaults**, and the trace shows `allocator_traits<A>` → `allocator_traits<allocator<int>>`.

**Round 7 did (2)**: `a_slot_matches` matches a template-id slot by **head (last name segment) and then its
arguments**, recursively — `allocator<_Ty>` against `std::allocator<MarkEvent>` is the same class written with the
qualification the use needed, and text equality could never see it. The fixture still answers
`allocator_traits<A>::size_type`, so (3) is now the only blocker: the walk reaches
`allocator_traits<allocator<int>>` and asks it for `size_type`, whose two `conditional` branches declare it
differently — the member list is honest about the ambiguity and the trait has to be **decided** before a branch is.
That means evaluating `_Is_default_allocator<std::allocator<int>>::value`: match (2) selects the specialization, and
the answer is the `true_type`/`false_type` base it inherits. That is the next round.

## A partial specialization whose first slot is a template-id gets no fact — open

Found while closing the `size_type` chain, and **corrected twice**, so the wrong readings are recorded here too:

1. The first reading was "the parser truncates the class head at `typename`". It does not: `inspect tree` prints a
   node's text cut at about sixty characters, and the cut landed on a token boundary. The tree for
   `struct B<A<T>, void_t<T>> : A<T> {}` is complete, `StructDef` and all.
2. The second reading was "no partial specialization produces a fact". That was the fixture's fault: two
   declarations had been concatenated onto one line, so the second was never a declaration.

What the fact builder actually does, with `inspect facts` on a minimal file:

```text
template <class T> struct A {};
template <class V> struct void_t {};
template <class T, class = void> struct B {};
template <class T> struct B<T, void_t<T>> : A<T> {};      →  B type over [T]        the specialization IS recorded
template <class T> struct B<T, int>      : A<T> {};       →  B type over [T]        recorded
template <class T> struct B<A<T>, void_t<T>> : A<T> {};   →  only the primary       NOT recorded
```

So the trigger is narrow, and **round 18 narrowed it further** — the first reading ("first slot is a template-id")
is not it, because `struct B<A<T>, A<T>> : A<T> { using inner = T; }` **does** produce a fact (`4:27 B`, with its
member). The shapes measured with `inspect facts`:

```text
B<T, void_t<T>>                       fact ✔
B<A<T>, A<T>>                         fact ✔
B<T, void_t<typename A<T>::From>>     no fact ✘
B<A<T>, void_t<A<T>::From>>           no fact ✘
B<A<T>, void_t<typename A<T>::From>>  no fact ✘
```

What separates them is a **qualified dependent name inside the specialization's argument list** — `A<T>::From`, with
or without `typename` — while a bare `A<T>` in the same position is fine. Facts are built from **bindings**
(`build_scopes` → `sema/declarations.rs`), so the missing fact means the binding was never created, and the next step
is `declared_name`/`name_node_along_declarator` on this head shape.

`_Is_default_allocator<allocator<_Ty>, void_t<typename allocator<_Ty>::_From_primary>>` is the failing shape in the
standard library, and its absence is what keeps `size_type` unresolved.

```cpp
template <class _Ty>
struct _Is_default_allocator<allocator<_Ty>, void_t<typename allocator<_Ty>::_From_primary>>
    : is_same<typename allocator<_Ty>::_From_primary, allocator<_Ty>>::type {};
```

— and the index holds no fact for it (`17000` is the principal, `17304` is the next declaration: nothing between).
`the_bool_a_trait_has` therefore falls back to the principal's `false_type`, `allocator_traits` takes the
`_Normal_allocator_traits` branch, and `size_type` is `typename _Get_size_type<_Alloc>::type` where it should be
`size_t`. The parser is not at fault; the next step is the fact builder's class-declaration path, where the head
with a template-id in its first argument is not being turned into a fact.

## Why `std::vector` itself is missing, and the wrong lever I pulled at it

This section replaces an earlier conclusion in `docs/plan-three-fixes.md` ("the initial pass builds summaries without
their closure's macros, so the refinement has to be taught to build them"). That conclusion was **wrong about the
lever**, and the measurements below are what showed it.

### What is reproducible, and by which path

| asked by | answer about `LuaCodeFormat.cpp:304` |
| --- | --- |
| `Session` directly (`real_project_probe.rs`) | `result`: `Unknown(UnknownType("std::vector<LuaDiagnostic>&"))` |
| the **real binary** over stdio (`real_project_hover_probe.rs`, LSP `textDocument/hover`) | the same string, as a hover card |
| the same, on a copy of the project with **no `.cppls`** (546 summaries built cold) | the same |

So the earlier finding was not an artefact of `inspect` (which cannot see includes), nor of the disk cache.

### Where the `std` scope actually is lost — measured, in three steps

1. **The summary of `<vector>` has no scope at all.** `993 fact(s), 0 with a std scope, namespace fact(s): []`, and
   each `vector` fact is `scope: None`. `macro_readings: 0` — the plain reading never saw `_STD_BEGIN` as a macro.
2. **That is by design, and the scoped reading lives elsewhere.** A summary is the shape-only reading
   (`docs/indexing-design.md` §2.1); the reading with bodies in force is the **cooked** one, built by the second pass.
3. **The second pass is demand-driven, and nobody demands `<vector>`.** `Session::advance`'s `finish_a_slice` takes
   `take_the_files_wanted_for_refinement(RE_READ_SLICE)` — files somebody called `want_refinement` for — and
   `catch_up(path)` asks for *the file a request is about*, never for the header that declares what it names. Traced:
   `re-read: eager false, 216 source(s), 175 word(s) mentioned, 2 candidate file(s), uncovered true, nothing bodied`
   — one pass, for the `.cpp`, and `<vector>` is not in it.

### The proof that the machinery is fine and only the ask is missing

`real_project_probe.rs::does_refining_the_header_itself_scope_it` indexes `<vector>` **as a root**, which is the one
thing that makes somebody ask for it:

```text
re-read: … 1153 word(s) mentioned, 12 candidate file(s), 12 bodied name(s) — 329 ms
re-read: …\include\vector through a walk of its own
[header-probe] std::vector -> …\include\vector (kind Type, scope Some("std"), definition true)
[header-probe] std::vector -> …\include\vector (kind Type, scope Some("std"), definition false)
[header-probe] std::vector -> …\include\vector (kind Type, scope Some("std"), definition true)
```

The class resolves, at `scope: "std"`, with its definition — after one refinement pass that was *asked for*. The
summary stays shape-only (`0 with a std scope`); the scope is in the cooked layer, which is what a lookup prefers.

### So the fix is on the ask, not on the summary builder

`Session::want_refinement` is `&self` and cheap (a set insert behind its own lock), and `take_the_files_wanted_for_
refinement` serves it in the next idle round. What is missing is a caller: when a lookup answers `NotDeclaredHere`
for a **qualified** name, the files that declare its **last segment** are exactly the files worth asking about, and
`ProjectIndex::every_fact_named` already answers which they are. Two constraints on that change:

* it must **not** parse the type spelling of the refusal (`UnknownType("std::vector<…>")`) to find the name — the name
  is known where the lookup happens, and a rule that reads a type back out of a message is the string-matching shape
  this crate removed once already;
* the first answer after the ask is still the shape-only one. A request that arrives before the refinement round has
  run answers `Unknown` and the *next* one answers; that is the same contract `is_owed_a_re_read` already states for
  a check, and it is why `handshake.rs` re-asks.

### The reproducer that already existed

`crates/cpp_code_analysis/src/session/tests.rs:3777`,
`a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session`, is `#[ignore]`d for this exact shape
(`_STD_BEGIN` defined in `open.h`, used in `widget.h`). Its reason cites `docs/buglist.md §33` and
`docs/indexing-review.md §6` — **both files were deleted** (`3d954caf "clean doc"`, `de501b6f "clean docs"`), and the
recovered `buglist.md` has no §33; the closest record is §4, "the macro environment a written view wants is never
built by the pump". A citation to a deleted document is not a record, and the ignore reason should either be
re-pointed at a live document or the test un-ignored once the ask exists.

## The hover that drew one member thirteen times

Reported with a screenshot of `CodeFormatCLib.cpp`: hovering drew a popup holding thirteen-odd identical lines
`std::size_t EndLine` and a scrollbar — a "very mysterious overload" about a data member. The file is four functions
over `FormatRange`, whose `EndLine` is one `std::size_t` in `CodeFormatCore/Format/Types.h`.

### What was measured, in the order it named the defect

Every name on the reported screen was asked of the **real server over stdio**, then every byte offset in the file
(1 600-odd hovers, 133 distinct answers): no answer held the popup, because a freshly started server answered one
declaration. A `Session` opened with `add_project_files` answered the same — **6** declarations in `FormatRange`, one
named `EndLine`.

The difference is `did_open`. With the file opened the way an editor opens it, the same session answered
**108 declarations — 18 named `EndLine` — all at the same ranges in `types.h`**, and `members_of` returned 108
members; `member_markdown` prints one line per candidate and does not de-duplicate, which is the popup exactly. The
copies were *not* duplicate postings: the scope's posting list held 18 distinct slots, 25 declarations apart, so one
**reading** of the file contained eighteen copies of the same 25 declarations.

The rendering of that translation unit said the same thing: `struct FormatRange` appeared **18 times** in
`RenderedUnit::text`, `types.h` was a file entry **20 times**, and the stream was 919 228 tokens and 1 404 file
entries — against **742 907 tokens, 206 entries and one copy** from the same project with an empty cache.

### The cause: `visit_once` was never written to the cache

`SummaryGuards::visit_once` is what the walk reads to decide that a second `#include` must not re-enter a file. It was
recorded on a unit's *frame* (codec version 24) and on the summary in memory — and the file summary's encoder never
wrote it, so every summary decoded from `.cppls` said `false`:

```text
stored summary:   SummaryGuards { regions: [], conditionals: [], own_guard: None, visit_once: false }
fresh summary:    SummaryGuards { regions: [], conditionals: [], own_guard: None, visit_once: true  }
```

A cold start therefore entered `#pragma once` headers once, and a warm start re-entered each at **every** inclusion.
`EmmyLuaCodeStyle`'s headers all begin with `#pragma once`, so one header included through eighteen paths declared
its six members eighteen times, and the member list — and the hover — showed it.

### The fix

* `summary_codec` writes and reads `visit_once`, `CODEC_VERSION` 38 → **39**, and the `every_field` fixture now sets
  it to `true` so the round-trip test fails if the encoder stops writing it.
* `READING_FINGERPRINT` folds `CODEC_VERSION`, so a record whose *shape* changed also moves the key of everything
  derived from it — a rendering, a unit's facts, a cooked reading, a walk contribution. Measured before this: a
  rendering kept answering after the summary reader was fixed, because nothing about the closure's *texts* had moved.
* `cache::a_rendering_key` and `walk_cache::key` fold `READING_FINGERPRINT` for the same reason at their own layer: a
  rendering and a walk contribution are functions of the reader as well as of the bytes.
* The unit's per-file merge files a declaration **by where it is written**, not by inclusion: `(range, name_range)` is
  the identity, so a stream that holds one header twice declares its members once. Regression test:
  `translation_tests::a_header_the_stream_holds_twice_declares_its_member_once` (it fails with `2` classes against the
  old concatenation).

### After the fix, measured on the same polluted cache

```text
[guard]  SummaryGuards { …, visit_once: true }     (twice: written, then read back)
[render] 742 907 token(s) of text, 206 file entr(ies)
[render] `struct FormatRange` appears 1 time(s); Types.h is a file entry 2 time(s)
[dup]    declarations_in(FormatRange): 6 declaration(s) … members_of(FormatRange): 6 member(s)
[clib-probe] 17:10 `EndLine`: "std::size_t EndLine — FormatRange::EndLine — a variable of type std::size_t, in FormatRange"
```

The probes that carry these numbers are `real_project_probe.rs::what_guard_the_cached_summary_records`,
`…::does_the_cached_rendering_hold_the_header_eighteen_times` and `…::where_the_duplicate_declarations_come_from`.

## Two checks that underlined correct code

Reported with two screenshots: `missing-include` on `FunctionSingleArgSpace` in
`CodeFormatCore/include/CodeFormatCore/Config/LuaStyleStruct.h`, and `dangling-field` on `spell::Word`'s
`std::string_view Item` in `CodeFormatCore/include/CodeFormatCore/Diagnostic/Spell/Util.h`.

### What the project said

Every check over the project's own **198** files produced **four** findings from these two rules, and all four were
wrong:

```text
missing-include  LuaStyleStruct.h               `FunctionSingleArgSpace` is declared in luastyleenum.h, which this file does not include
missing-include  CodeFormatLib/src/Types.h      `DiagnosticType` is declared in diagnostictype.h, which this file does not include
dangling-field   Spell/Util.h                   `Item` is a `string_view` initialised from `item`, a parameter that is destroyed …
dangling-field   Util/include/Util/format.h     `Expr` is a `string_view` initialised from `expr`, a parameter that is destroyed …
```

### Why the `missing-include` findings were wrong

A header is not compiled on its own: its text is spliced into whichever translation unit includes it, and that file owns
the include order. `LuaStyleStruct.h` uses `FunctionSingleArgSpace` and includes neither it nor `LuaStyleEnum.h` — and
the project compiles because `LuaStyle.h` includes the enum on line 4 and the struct on line 5. `CodeFormatLib/src/Types.h`
is the same shape: `LuaCodeFormat.h` includes the diagnostic headers before it. The rule's premise ("the program compiles
only because another translation unit's include order happened to pull that header in first") is a statement about a
**translation unit**, and a header is not one.

### Why the `dangling-field` findings were wrong

`Word(TextRange range, std::string_view item) : Range(range), Item(item) {}` copies a *view*: the parameter object dies
at the closing brace, and the characters it names are the caller's. The rule treated every by-value parameter as a copy
that dies with its storage — which is true of `S(std::string s) : v(s)` and false of a view or a pointer, and the
difference was already known to the same analysis: `Borrowed::owner_is_a_view` exists because a view *local* was
reported as "a local that is destroyed" in `dangling-return`.

### What the rules do now

* `missing-include` in a file a compiler does not compile on its own is about **the files that include it**: the name is
  missing only if some includer does not already reach the declaring header (`an_includer_misses`). A header nothing
  includes is still reported, because nothing makes the name visible there.
* `dangling-field` stays silent for a **view member initialised from a by-value parameter that owns no storage** — a
  view or a pointer. A reference member is unaffected (binding a reference from a view would be a conversion temporary),
  and `S(std::string s) : v(s)` still reports.
* The `Borrowed` flag is renamed `owner_owns_nothing` and computed for locals (views only — a *local* pointer is exactly
  `dangling-return`'s question) and for by-value parameters (views and pointers).

### After

```text
[checks] missing-include 0, dangling-field 0        (the same 198 files)
```

And the editor's own request, `textDocument/diagnostic`, asked of the real server for the two reported files:
`LuaStyleStruct.h` → 0 items, `Spell/Util.h` → 0 items
(`cpp_ls/tests/real_project_hover_probe.rs::the_two_reported_files_have_no_finding_from_the_server`).

The true positives stay covered: `dangling_field::a_view_member_of_a_by_value_parameter_is_reported`,
`…::a_pointer_member_of_a_parameters_buffer_is_reported`, and `missing_include::a_header_inside_its_includers_order_is_silent`'s
second half, where the same fragment is included by a file that never reaches the enum and is still reported.
`real_project_probe.rs::what_the_two_reported_checks_say_about_the_project` prints the count.

## 5 GB of private bytes from opening nineteen files

**Reported**: opening `C:\Users\zc\Desktop\test_cpp` made the server's memory grow without bound, especially when
`basics.cpp` was opened; the process was seen at **5 037 MB**.

**Measured, and the shape is per opened file.** Each opened file gets one *whole-program read*
(`Session::the_program_to_read` → `Session::read_the_unit`): the whole include closure is rendered, parsed and swept
for facts. On this project a closure is a handful of standard headers (~6 MB of source). Sampling the server's own
process (`cpp_ls/tests/memory_probe.rs`, which opens every `.cpp`/`.h` and pulls a diagnostic per file):

```text
 8 files opened    951 MB private
19 files opened  5 037 MB private          (the report)
19 files, after the budget below   713 MB private / 436 MB working set
```

The session's own tables during that growth, from `CPPLS_TRACE_MEMORY=1` (a pull prints `Session::memory_report`):

```text
summaries 663 (64.5 MB) | units 8 (32 MB) | cooked 136 (7.9 MB) | views 8 (2 MB)
| index tables 12–23 MB | total ≈ 150 MB
```

So the tables are bounded and the growth is not in them. It is in the program read: cutting the reads from nineteen
to one cuts the process from 5 037 MB to 713 MB — the budget experiment is the causal one.

**Not the allocator.** The server runs **mimalloc** (`crates/cpp_ls/src/bin/cpp_ls.rs`), which does return freed
pages, so "the heap kept the slack" is the wrong explanation, and the first version of this section said it. Whatever a
read allocates is still *reachable* when it is measured — ~280 MB per read.

**What is still open**: *what holds it*. The report's cooked readings grow by ~10 per pull, so `Readings::in_a_program`
(one `CookedFile` per file per program, unbounded) is not the answer for these files; the candidates left are the
`TranslationUnit` the read builds (32 MB accounted, `heap_bytes` may undercount), the `IndexedUnit` memo, and the
server-side queues. The next measurement is the resident set sampled **at the `read_the_unit` boundary** with the table
sizes printed either side, which names the holder in one run.

**Fixed meanwhile**: `PROGRAM_READ_BUDGET_BYTES` (512 MB, estimated at 48 bytes per source byte, so about one
standard-heavy program). Beyond it a file keeps the per-file reading — the reading every file nobody has opened
has — instead of a second whole-program read. Two tests: the budget refuses (`a_session_stops_reading_programs_once_
its_memory_budget_is_spent`) and a read is charged (`a_program_read_is_charged_to_the_budget`). The cost is real: the
second and later standard-heavy files of one session lose the compiler's whole-program reading, which is where
cross-file macro scoping is decided. Removing the budget is the goal once the holder is found.
## Two includes behind a `HAVE_…` guard still warn

**Measured on 2026-11-13, and the answer is not the one the last edit of this section guessed.** The two
remaining `include-not-found` findings in the neovim census are `<execinfo.h>` (`log.c:43`) and `<langinfo.h>`
(`mbyte.c:2389`), behind `#ifdef HAVE_EXECINFO_BACKTRACE` and `#ifdef HAVE_LANGINFO_H`. The generated
`build/cmake.config/auto/config.h` has both as `/* #undef … */` — comments, so nothing defines them — and the
question was whether the check could decide that.

`tests/guard_probe.rs` answers it:

```text
[guard] the compilation environment is complete (a name nobody mentions is not defined)
[guard] src/nvim/log.c: execinfo.h guard Region(0) -> taken=true; HAVE_EXECINFO_BACKTRACE: unknown=true mentioned=false
[guard]   walk state: complete_before=false incomplete_here=true base_complete=true
```

So the compile environment **is** already what the check uses (`ProjectIndex::macros()` — the `-D`s and the
compiler's `-dM` table — is what `macros_at` starts from), and passing it in is not the missing piece. What makes
the guard `Unknown` is that the **walk takes the completeness claim back**: `macro_candidates` calls
`mark_incomplete` when an `#include` it reaches does not resolve, and an unreadable file "may define anything", so
from there on a name nothing mentions is `Unknown` rather than undefined. `log.c`'s closure reaches a header that
does not resolve on this machine, and the state at the guard is already incomplete (`complete_before=false`).

**Not fixed, and deliberately**: the honest reading of a missing header is "its macros may be anything". The
alternatives are to find every missing header (the census's own four include misses say the search is not
complete) or to narrow incompleteness to "only the names the missing header could define", which is the same
guess with more steps. The warning is correct as a warning — it is what this analysis's search says about a file
it could not find — and `include-not-found` is a `Warning` for exactly that reason. The census's two findings are
therefore the honest floor until the include search finds everything on the machine.

Test that pins the behaviour: `an_include_behind_an_undecided_guard_is_still_reported`.
