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

## A partial specialization whose first slot is a template-id gets no fact — **fixed**

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

**Fixed, measured 2026-11-16.** The head shape now produces its fact: `inspect facts` on the installed `<xmemory>`
reports both declarations, the principal and the specialization.

```text
483:8  _Is_default_allocator        type over [_Alloc, ]
486:8  _Is_default_allocator        type over [_Ty]
```

The minimal shapes from the table above answer too — a fixture with the two `void_t<typename A<T>::From>` specializations
reports `3 of 6 declaration(s)`, the primary and both specializations. Which change fixed it is not recorded here (the
section above is the record of the readings that did *not*); the red it describes no longer reproduces, so the
`size_type` chain it blamed is a separate question, not this one.

## `std::vector` itself was missing because nobody asked for the header's refinement

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
2. **That is by design, and the scoped reading lives elsewhere.** A summary is the shape-only reading;
   the reading with bodies in force is the **cooked** one, built by the second pass.
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
## Two includes behind a `HAVE_…` guard still warn — the chain, measured

Two `include-not-found` findings survive the branch rule: `<execinfo.h>` (`log.c:43`) and `<langinfo.h>`
(`mbyte.c:2389`), behind `#ifdef HAVE_EXECINFO_BACKTRACE` and `#ifdef HAVE_LANGINFO_H`. `auto/config.h` has both as
`/* #undef … */` comments, so nothing defines them and a **complete** environment should decide both `#ifdef`s
false. Three measurements say why it does not.

1. **The environment is complete and already what the check uses.** `CPPLS_TRACE_MEMORY`'s sibling
   `tests/guard_probe.rs` prints `the compilation environment is complete`; `macros_at` starts from
   `ProjectIndex::macros()` (the `-D`s plus the compiler's `-dM` table). Passing the environment in was never the
   missing piece.
2. **One unresolved `#include` takes the completeness claim back.** `macro_candidates` calls `mark_incomplete` for a
   directive it cannot resolve — a header it never read may define anything — and from then on every unmentioned
   name is `Unknown`. `tests/guard_probe.rs` lists **26** such directives in `log.c`'s closure.
3. **The roots are guards the walk cannot evaluate.** `CPPLS_TRACE_GUARD=1` shows MinGW's own headers asking
   `__MINGW_GNUC_PREREQ ( 4 , 1 )` and the walk answering `None` — its `MacrosHere` knows a macro's *name* and a
   command-line *value*, not the body of a **function-like** macro. So branches this machine does not compile are
   entered, and the includes inside them (`sys/socket.h`, `mach/mach.h`, `sys/sdt.h`) are “not found”.

**Fixed in the same round, one layer down**: a header now borrows the include directories of the source beside it
(`CompileCommands::a_command_for_a_header`, `FORMAT_VERSION` 25). A build lists only sources, so
`src/nvim/os/fs.h` had no command and no `-isystem .deps/usr/include` — **eleven** headers of the closure
reported `<uv.h>` unresolved. After the fix the closure's unresolved directives went **32 → 26** and every
`uv.h` one is gone (6 of them); the remaining 26 are the platform headers above.

**Not fixed**: giving the walk's condition evaluator the *bodies* of function-like macros. `MacroFact` already
carries `body_text` and `parameters`, and the cook expands them (`MacroView`); the walk's evaluator does not, which
is why MinGW's guards are `Unknown`. That is the next change, and it would let `#ifdef HAVE_*` be decided on this
machine — and with it the two findings.

Tests pinning today's behaviour: `an_include_behind_an_undecided_guard_is_still_reported` and
`a_header_borrows_the_include_directories_of_a_source_beside_it`.


## `containers.cpp` grows private bytes to gigabytes with no request at all — measured

Reported: opening `test_cpp/src/containers.cpp` (seventeen standard headers) balloons the server. Measured through
the real server (`crates/cpp_ls/tests/memory_probe.rs`), opening that one file and asking **nothing**:

```text
  0 s   254 MB
 10 s   600 MB
 20 s   900 MB
 35 s  1 375 MB
 55 s  2 553 MB   (killed by the probe's guard)
```

With requests it is faster (`12` rounds of eight requests took it to **7 500 MB**). The session's own tables stay
near **40 MB** the whole time (`summaries 17.4 MB`, `views 4.6 MB`, `index tables 2.3 MB`), so the bytes are
transient — something the pump does every round allocates and drops, and the allocator keeps the peaks.

**What the pump is doing, in process** (`crates/cpp_code_analysis/tests/idle_pump_probe.rs`, a counting
allocator): after the closure was walked (663 summaries),

```text
queue 0 (open 0, rest 0), refinements wanted 67
reached idle in 10 000 rounds, 13 727 ms, pending 67   ← never reaches zero
every later advance(256): 9 936 allocations, pending 67
```

`pending_work()` is 67 and the index queue is empty, so the pump never idles: each round prepares a wave for the
open file's closure, and the refinement fast-path misses (`the_stored_refinement` → "not stored") **847 times in
35 s** while only **106** distinct files are ever cooked (a finite, one-time pass — the cooks are not the leak).
The server's own no-progress breaker (`initialized/mod.rs`, eight slices with nothing built) drops the cooking
queue, but the asks that keep `pending_work()` above zero are **not** the cooking queue, and nothing refuses them.

**A real defect found on the way, fixed**: the view cache held whole-unit rendering views (measured at ~75 MB of
tree for this file) in the same eight-entry cache as buffer views, so ten cheap views evicted the expensive one and
every request re-parsed the unit. `Cache::rendering_views` is now a cache of its own (`RENDERING_VIEW_ENTRIES`,
`RENDERING_VIEW_BYTES`), pinned by
`session::tests::a_rendering_view_survives_twelve_buffer_views` — which fails on the shared cache. It does **not**
explain this report's growth (the unit's rendering here is 3 205 bytes, because its includes are not inlined), so
the growth is still the pump's.

**Next**: the refinement asks have no `cooking_refused` equivalent. A file the pass cannot serve is asked for again
on every drain, and each drain pays a wave preparation over the closure. Refuse an ask that has been taken and
served without `is_refined` becoming true (per content, cleared on edit — the rule `cooking_refused` already
follows), so `pending_work()` can reach zero; and call `mi_collect(true)` after a closure cook burst, since the
peaks are what is being kept.


**Second round, measured** — the holder is outside every table the session reports, and the growth scales with
the closure:

```text
src/main.cpp        (few includes):  flat at ~510 MB for 20 s idle
src/containers.cpp  (17 headers):    2 553 MB in 55 s idle
at 2 334 MB, a diagnostic pull reports: vfs 19 files, summaries 19, tables ~2 MB
```

So the session holds **19 files and ~2 MB** at the moment the process holds 2.3 GB. What *is* specific to
`containers.cpp` is its closure: the pump's `want_the_closure_cooked` names its seventeen direct includes on every
drain ("not read, cookable, not refused" — the prefix stays in `parsed_since_the_last_pass`), and
`prepare_wave`/`prepare_the_re_read` pay a **closure-fingerprint walk per file per round** (up to 8192 files
each), with the refinement fast path missing every time (847 `skipped: not stored` in 35 s).

Tried and measured as **not** the cause, and reverted rather than kept:

- `mi_collect(true)` from the pump and from the blocking threads that cook (7 calls in 10 s): no change;
- redb's page cache (default **1 GiB**; now capped at 64 MB — kept as a bound, but it moved this number not at all);
- making the pump wait instead of yield when its backlog survives the no-progress breaker: no change (the breaker
  was not the spin).

Next: a heap profile outside the session (mimalloc's own stats, or a sampling profiler) to name the allocation
site, and a bisect of the seventeen includes (a file including only `<map>`, only `<variant>`, …) to find whether
one header triggers the loop.


**The trigger, bisected to one statement** (temp files under `test_cpp/src`, deleted afterwards; the server opened
one file and asked nothing for 15 s):

```text
17 includes + one trivial function              flat   ~535 MB
the same with only the first 8 / only the last 9 flat   ~559 MB
17 includes + sequences() and associative()     flat   ~518 MB
17 includes + `struct Value` (std::visit)       flat   ~505 MB
containers.cpp without vocabulary()             flat   ~568 MB
vocabulary() alone, in its own file            2 555 MB
      its optional+variant half                flat   ~539 MB
      its any/tuple/pair half                  2 512 MB
            std::any alone                      flat   ~565 MB
            the tuple statements               2 595 MB
                  `tuple_size_v<decltype(t)>`   flat   ~548 MB
                  `std::get<0>(tuple)`          2 585 MB   ← the trigger
```

So the smallest reproducer is a file with the seventeen includes and

```cpp
std::tuple tuple{1, 2.5, std::string{"three"}};
std::cout << std::get<0>(tuple);
```

`inspect diagnostics --index` on that file is **fast and quiet** (215 MB, 0 findings), so the loop is **not** in
the check layer: it is in the server's pump path that the standalone tool does not run. The next question is
therefore the pump's own, and the probe that answers it is one request order away: does the cook of that file
land (`cooked_declarations` `Some`) or is it re-wanted every round? The in-process pump probe cannot see it — it
does not cook at all (`cooking 67, cooked 0`), because cooking belongs to the server's `claim_cooking`/
`render_cooks` path.


**Third round — two fixes landed, and the residue is now one request.**

Fixed and measured:

- **The pump's no-work round waits.** It used to `yield_now()` and continue, so a backlog it could not move spun at
  thousands of rounds a second. With `CPPLS_NO_COOKS` set, that loop took private bytes 476 MB → 1 228 MB in eight
  seconds **while every stage the pump measures allocated 0 MB**; with the wait the same scenario holds **flat at
  422 MB for fifteen seconds**.
- **A stored-refinement miss is remembered** by `(path, content hash)`. `prepare_wave` asked the disk cache for the
  slice's files on every round — a whole-closure walk per file, and a miss that cannot change for a text (nothing
  is storable). Measured: `wave 20 MB` on every round with `prepared 0`; now `wave 0 MB`.

The residue, bisected to one request:

```text
idle, no request            flat at 422 MB for 15 s      (after the fix)
one textDocument/diagnostic grows ~90 MB/s, one core at 100 %, 21 threads
                            pump rounds < 100/s (not the pump), no watcher, no cooks,
                            every pump stage 0 MB, session tables 66-73 MB
```

So the bytes are in the **request path** after the pull, not in the pump and not in the checks:
`inspect diagnostics --index --cook` on the same file is 212 MB and instant. The next instrument is the same
allocation counter around `AnalysisState::catch_up`, `AnalysisState::prepare` and `Session::diagnostics`, printing
per request — that names the call in one run.


**The pull path, split** (the allocator counter that printed `[pull]` has since been removed; `CPPLS_TRACE_QUERY`
still prints the `[query]` half):

```text
[pull] prepare done, 117 MB       ← prepare_for_a_diagnostic, per pull
[pull] diagnose_file asked for 5 MB
[pull] query returned, 8 MB after the prepare
[pull] prepare done, 0 MB        ← the second pull, and then no further line
```

So a pull is not the 90 MB/s — it allocates 117 MB once, in `prepare_for_a_diagnostic` — and the growth after it
is something that starts then and keeps going: with the pump waiting (the fix above) its rounds allocate 0 MB,
`pending` stays 66, and no stage runs; `[query]` lines show every query it did run at 0–6 ms. The next question is
what runs at 100 % of a core *outside* both: the trace to add is thread-level, not stage-level.


**The livelock, found and fixed.** `commit_a_wave` broke out of its commit loop when the head of the queue was an
open document with no prepared reading — and `prepare_wave` *defers* open documents, so it never has one. Nothing
committed, the head was never popped, and the pump prepared the same wave for ever: measured after one diagnostic
pull on `containers.cpp`, **17 rounds a second, one full core, ~90 MB/s**, `pending` stuck at 66 with every session
table at 66 MB. `index_one` builds the summary itself when the wave left the file alone — the treatment `advance`
already gives it — so the guard is gone. Measured after: `pending` drains 66 → 8 → 4 → 0.

What is left is the **peak**, not a loop: with `pending` at zero the process still reached 2.8 GB over 40 s, and the
pump's rounds at that point allocate nothing measurable — the last unexplained number is ~16 `query_blocking` calls
a second with no request arriving, which is either `refresh_pull`/`schedule_open_files` re-running or a query's own
work; the next instrument is those two calls with the counter.

**Fourth round — the peak was the program reads, and the budget was never paid.**

The instrument that named it was an experiment switch: `CPPLS_NO_PROGRAM_READ=1` made the twenty-open-file run
**flat at 511 MB for 60 s** on the same tree that grew to 1 094 MB in 34 s without it. A whole-program read is the
one operation that allocates hundreds of megabytes at once, and `PROGRAM_READ_BUDGET_BYTES` (512 MB) exists so that
twenty open tabs do not cost twenty of them — but the charge lived in `Session::read_the_unit`, which is **not the
path the pump takes**: the pump goes `AnalysisState::read_a_program` → `Session::commit_a_program`, and that path
charged nothing, so every open file's program was read however much the previous ones had cost.

Three changes, measured after a cold cache with a watchdog that samples private bytes every 200 ms and kills at
1 GB or two minutes — the harness has a bound because the bug is unbounded:

```text
                                       before                     after
twenty files open, no request    1 094 MB at 34 s (killed)    515-550 MB flat, 60 s
one file + the editor's requests  604 MB peak at 1 s, exits in 6.5 s
CPPLS_NO_PROGRAM_READ on that run   511 MB flat for 60 s
```

- **The budget is charged in `commit_a_program`**, the one place every path goes through. Test:
  `session::tests::a_program_read_charges_the_budget_that_bounds_it` — 0 charged on the previous tree.
- **A declined program read waits.** `read_a_program` answers `false` when the file it chose is over budget, and
  such a file is *deliberately* re-considered on every drain; the pump used to `continue` with no wait, and
  re-considering walks the closure of every open file to price it. Twenty tabs spun on twenty closure walks.
- **A file is asked to be cooked once per text.** `want_the_closure_cooked` runs per open document per drain and
  asked the index's *bounded* cooked map, so an evicted reading read as "nobody cooked this" and the file was
  queued again: 486 cook commits for 179 distinct files in 33 s, before the fix. The ask-once set is cleared where
  a file's text moves (`buffer_changed`) or its dependents are invalidated.

Left open: the first programme read of a standard-heavy file still peaks around 600 MB, which is the feature's own
cost (one parse of the whole program) rather than a leak; a project with twenty tabs open now pays for one such
program and keeps its own reading for the rest.

## `std::println` shows only two declarations, both from `<ostream>` — **fixed**

Reported from `test_module`'s `std_module_demo.cpp` (GCC 15 / MinGW): hovering `std::println("…")` drew
`→ inline void println(ostream& __os)` out of the standard library's six overloads, and the parameter hints labelled a
string literal `__os`. Three faults stacked, and each rule is now stated where the question is asked.

1. **A class query was asked about a namespace.** The answer came through `members_of(index, …, "std")` — a *class*
   member list, which returned the declarations of whichever file declared `std` first (two, both `<ostream>`). The
   branch exists for `Derived::instances`, a class member reached through a base; it now asks
   `ProjectIndex::scope_names_a_class` first, and a namespace falls through to the name lookup.
2. **A reading that compiled nothing shadowed the summary.** With the branch skipped, the walk still answered two:
   `the_reading_for` selected the asking program's reading of `<print>`, and that reading held **zero** declarations
   (the program's parse nested `<print>` inside a `<mutex>` function body — the file's own text balances, the pairing is
   what a cross-file `{` … `}` can still do; the parse is one stream and the boundary rule only closes what a block
   loop reaches). The summary held all forty-one. The reading-selection rule is now: an **empty** reading cannot
   answer — the asking-program-wins policy is about a reading that *has* declarations and lacks this name
   (`#if 0 struct Dead`), not about one that holds nothing.
3. **The conversion relation kept every pointer overload a candidate.** With all six found, `println("…")` still
   matched `println(ostream&)` and `println(FILE*)`, so the selection was ambiguous and the card listed five candidates
   with no `→`. `Type::convertible_to` now refuses two shapes it can decide: a pointer against a non-const lvalue
   reference (a temporary cannot bind to one), and a pointer to a builtin against a pointer to a different class or
   builtin (`const char*` is not `FILE*`; class-to-class stays `Unknown`, because a derived pointer converts). One
   viable candidate remains — `println(format_string<_Args...>, _Args&&...)`.

Measured through the real server over stdio (`hover_probe`, `test_module`), after the three:

```text
→ inline void println(format_string<_Args...> __fmt, _Args&&... __args)

`std::println` — 6 declarations. The `→` one is what this call means: its parameter list accepts the arguments, and the
others cannot. `go to definition` lists them all.

Declared in `print:130:5` and 1 other file(s)
```

and the parameter hints are `__fmt:` on every `println` call in the file (the `inlay` query lists six hints), where they
were `__os:`. The conversion rules have their own test —
`sema::types::tests::a_pointer_does_not_bind_to_a_non_const_reference_or_to_another_pointers_class`; the other two rules
are covered by the existing hover and visibility suites (996 analysis tests, the whole workspace green).

Also fixed in the same round: the two declaration sentences in `overloads_markdown` carried a literal `\` and the next
line's indentation — `\\` in a Rust string is an escaped backslash, not a line continuation — so the reported card read
"and the others \ cannot".
## The program parse can nest one file inside another file's function body — open

Measured while chasing `std::println`. One parse of a 5 MB `std` program (5 054 878 bytes, 1 631 frames, braces
balanced, `unbalanced` empty) placed `<print>`'s `namespace std { … }` inside a `<mutex>` `if constexpr` body:

```text
Syntax(Declaration) "template<typename... _Args> inline void println(FILE* __stream, …"
Syntax(CompoundStat) " { inline void vprint_nonunicode(FILE* __stream, …"
Syntax(NamespaceDecl) "namespace std { inline void vprint_nonunicode(…"
Syntax(CompoundStat) " { template<typename _Key, …"
Syntax(NamespaceDecl) "namespace std { template<typename _Key, …"
Syntax(IfStat) "if constexpr (is_same_v<_L1, _L2> && …)"
Syntax(CompoundStat) " { if constexpr (…) { …"
```

The consequence was that the four `<print>` declarations never became facts: `[attr]` saw **two** `println`
declarations, both from `<ostream>`, and the file got a cooked reading of 0 declarations against a summary of 41. The
hover is right now because an empty reading no longer shadows the summary (see the fixed section above), **not** because
the parse is right: a file that loses *some* declarations to the same pairing has a non-empty reading, and that reading
still speaks alone.

Why the file-boundary rule does not close it: `parse_stats` stops at a boundary it has crossed, but the statement
being parsed when the crossing happens — here the `if constexpr` and its body — consumes the next file's text before
the loop can ask again. The check sits at the top of the loop, and the damage is done inside one iteration. A fix has to
be a boundary rule no *construct* can consume across (a token-level barrier) or a rule in the statement recursion, not
another tree-builder rule; `RenderedUnit::file_boundaries` records the earlier attempt and why closing nodes does not
move the shape.

## The 4.5 GB is one inlay-hint request, and every ceiling above was aimed at the wrong thing

Every memory bound in this codebase was justified by a number read off a counter that **only ever added** — no free was
subtracted — so it measured *churn* and was compared against the resident set. A counter that counted both sides
(`cpp_code_analysis::alloc`, since removed with the global allocator that fed it) was written for this section, the
`live`/`peak` table below is what it measured, and `crates/cpp_code_analysis/tests/cold_start_memory_probe.rs` (also
removed) drove a whole session with it.

**What the session actually holds** (`test_cpp`, live bytes, not churn):

| run | live | peak | the session's own tables |
| --- | --- | --- | --- |
| cold index (from the database) | 147 MB | 147 MB | 65 MB summaries + 10 MB index |
| 17 files open, pump drained | 250 MB | 281 MB | 98 MB |
| + 17 whole-program reads | 478 MB | 528 MB | 196 MB |

A program read costs **2.7–4.4 s**, 340 000–594 000 tokens and 163–213 files, and adds ~13 MB to live — bounded by the
caches. **Churn does not inflate the resident set**: forty reads of a 218 KB header churn **1 723 MB** in 32 million
allocations and the working set never left **11 MB**. So neither the indexer nor the program read is the spike, and
`WAVE_PREPARE_BYTES`, `PROGRAM_READ_BUDGET_BYTES`, the program-read token ceiling, the `IndexedUnit` memo cap and
`SLICE_ALLOCATION_CAP` were all bounding a number that is ~0.5 GB at worst. `SLICE_ALLOCATION_CAP` is deleted.

**What the spike is.** `crates/cpp_ls/tests/memory_probe.rs`, one file open (`src/containers.cpp`), a client's own
requests, the server's working set sampled from outside:

```text
semanticTokens/full       66 MB max
inlayHint                 99 MB → 2 059 MB, no answer for 22 s   (killed at 2 GB)
```

and with `CPPLS_TRACE_INLAY=1` the request is one call in `parameter_hints`:

```text
[inlay] resolving std::get<0>(tuple)      ← and nothing after it
```

The five calls before it resolve in microseconds to milliseconds. During all of this the session's own accounting reads
`live 19 MB, peak 67 MB, churn 167 MB, tables 3 MB` — so the gigabyte is inside *that one name resolution*, not in the
index and not in a program parse. It is also the shape an editor amplifies: a client sends an inlay-hint request per
visible-range change, so a request that takes 22 s and a gigabyte stacks up.

**Next.** Resolve a call with explicit template arguments (`std::get<0>(tuple)` against libstdc++'s overload set) in
time proportional to the candidates, and pin it with a test that asserts the answer *and* that the work is bounded (no
ceiling: the candidate set is finite and the answer is one of them). The `PROGRAM_READ_BUDGET_BYTES` pair is dead
weight and goes with the same change.




## The toolchain and the preprocessor are their own crate, so the cache has a unit to key

Every memory number in the section above was read from a process whose unit of work is a **whole program**, while its
caches are bounded per **file** (`renderings` 8 entries, `cooked` 512, `views` 8). The two do not fit: opening twenty
files ran **18 whole-program reads with no request at all**, 2 028 per-file readings inserted, 4.5 GB of churn in
twenty seconds, and the private bytes went 462 MB → 1 196 MB. Turning the program reads off pinned the same run at
596 MB. The repetition is not in a header's parse — the same `<vcruntime_new.h>` was parsed once per program, ten
times — it is that **nothing is shared between one program and the next**.

That is a design question, not a tuning one, and the answer starts by making the unit honest: a **file's rendering**
— its text with the environment applied, and the spans back to the file — is what a cache can key and hold, and it is
produced entirely by the toolchain. So the layer moves out:

```text
cpp_toolchain   project (`.cppls.toml`, compile_commands, CMake, the toolchain)   2 942 lines
                include (search order, resolver, graph, msvc, system headers)     8 162
                preprocess (directives, macros, conditions, guards, expand, cook) 6 591
                paths, token, probes                                              1 025
                                                                         total   18 720
```

and what is left in `cpp_code_analysis` is the index, its cache, the session, and the semantics — the layer whose
caches are the ones that do not fit.

**Three seams had to be cut, and each one is a dependency that should not have existed:**

1. `preprocess` asked `index::environment` two questions (`MacrosHere::from_walk`, `a_branch_is_taken`). The first was
   the seed answering for itself (`Marked::lookup`); the second — "is this `#ifdef` branch the compiled one" — moved
   to `cpp_toolchain::preprocess::environment` and is re-exported by the index, because the *walk* is its caller.
2. `preprocess::cooked::FileMacros` took the analysis's `MacroView` and `UnitDefinitions` by value. It now takes
   `&dyn MacroEnvironment` and `&dyn UnitDefinitionsView`, two methods each, implemented by those types in
   `summary.rs`: the renderer owns a file's text and expansion, the timeline owns where the file sits, and neither
   needs the other's types. This is also the seam the new cache is built on — a rendering is a function of *this
   file* and *this environment*, and both are now nameable without a session.
3. The index's `Marked::observe(&MacroFact)` was deleted with the walk, and the one caller now uses
   `index::environment::observe_a_macro_fact(&mut Marked, &MacroFact)` — the state is the toolchain's, the fact is
   the summary's, and the function between them belongs to the layer that owns the fact.

Two smaller ones moved with their owners: `CACHE_DIRECTORY` and the two hashes (`fnv1a64`, `content_hash`) are the
project's layout and the cache's key, and the toolchain needs them first; `PathPattern` stayed in the index, and the
config reader now validates a pattern with `glob::Pattern::new` directly, because *what a pattern must be* is the
glob grammar and *what it excludes* is the index's question.

**What did not change**: no behaviour, no test assertion. `cargo test -p cpp_toolchain` is 145 tests, the workspace is
**3 039 in 200 suites**, and every path written before the split still resolves — `crate::include::…`,
`crate::macros::…`, `crate::probes::…` are re-exports, not a second copy.

**Next**, and the reason the split was worth its churn: the cache and the index are rewritten in place. The unit is one
file's rendering, keyed by `(content hash, environment fingerprint)`; a program read becomes a list of those keys, a
second program with the same list does no work, and a query never asks for a cook because a cache missed — it returns
what it knows and the misses it needs, and the scheduler serves the focused document only. The budget is not a byte
ceiling read from a counter that only adds: it is `idle ⇒ zero allocations`, `a drained queue ⇒ zero cooks`, and a
peak per program read, all three asserted by tests.


## The crate is five layers, and every feature lives in `session`

The reorganisation after the crate split, and the same rule applied one level down: a file belongs to the layer that
*owns the question*, not to the layer that happened to call it.

```text
sema      35 653   names, scopes, types, overloads
file       1 781   one file analysed: the VFS, a parsed view, the coordinates between them
cache     12 146   the store and what it stores: summary, its two codecs, the byte counter, the alignment report
index     37 087   the summaries in memory, the store, the queries --- plus a file's macro effect and surface
session   29 683   a project opened, and every request a client can make
```

**What moved where.** `summary`, `summary_codec`, `unit_codec`, `memory`, `alloc`, `align` and `perf` are the cache
(what it holds, what it writes, and the numbers it reports); `effect` and `surface` are the index (what a file does to
the macro table, and the directives that do it); `completion`, `semantic`, `signature`, `inlay`, `definition`,
`folding`, `doc`, `format` and `cuda` are the session (they answer a cursor, and nothing below them asks them
anything).

**Two seams were not moves.** A file at the top level is not always a feature, and the test is the dependency:

1. **`inlay` was also a syntax reader.** The index asked it for a call's arguments (to choose an overload), `sema`
   asked it for a parameter list and its one-line text, and the completion layer asked it to read a list out of a
   fact. A feature module that the index depends on is a dependency pointing the wrong way, and it was invisible while
   both shared a crate. The five readers moved to `sema::call_syntax` --- the one place in this crate that reads the
   tree for its shape alone --- and `session::inlay` kept the hint layer. The style suite caught the one thing a move
   must not carry: `CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator`, which became
   `CppDeclarator::can_cast`, and `ArgumentList`, which has no typed wrapper and is now in the allowlist with that
   reason beside it.
2. **`session/completion` needed two halves in one directory.** The feature is the vocabulary and the answers
   (`completion/mod.rs` and its four submodules); the `impl Session` methods a handler calls are
   `completion/request.rs`, `use super::super::*` because one `super` is now the feature rather than the crate.

**What did not change**: no behaviour, no test assertion beyond the style allowlist above. 145 toolchain tests, 3 039
in 200 suites for the workspace, clippy clean, and `cargo check` green with and without `perf-probe` --- which was the
one feature that would notice a module being declared in the wrong place. (The feature has since been removed: its only
driver was one of the probes deleted in `2dc17c8d`, so every mark it recorded read a counter nothing incremented.)

**Next** is the same "next" as the section above, and this reorganisation exists to make it a small change: the cache
and the index are rewritten in place. A request returns what it knows plus the misses it needs; a program read becomes
a list of `(content hash, environment fingerprint)` keys; and the budget is `idle ⇒ zero allocations`, asserted.


## The identity of a rendering is its text and its reachable environment — measured

The cache rewrite starts from one number: **what two readings of the same file share**. A file's rendering is a
function of its own text and of the `#define`s in force where it sits, so the unit a cache can key is
`(content hash, environment fingerprint)`. `MacroView::environment_fingerprint` is that second half, and the design
question it answers is *which* macros count:

```text
the file's own identifier tokens   ->  the names it can mention
each name's definition in force    ->  name, function-like, parameters, body
each name inside that body         ->  the closure: `#define A(x) B(x)` means B's definition is part of A's meaning
```

A parameter of the macro is not one of those names (`#define B(y) y + 1` says nothing about a macro called `y`). The
seed — `-D` and the compiler's predefines — is deliberately not hashed: it is a property of the compilation, which the
key already carries as its context hash.

**Three properties, each with a test** (`tests/translation_unit.rs`):

1. two programs that include a header in the same state agree on its fingerprint, and one that defines what the header
   uses *before* including it does not;
2. a macro the file never mentions does not move the fingerprint, even when it is defined before the include in both
   programs — this is what makes sharing possible at all;
3. a definition reached through another definition counts: `chain.h` writes `A`, `A` stands for `B`, and a different
   `B` renders a different line — so hashing only `A`'s body would be the unsound half of the shortcut.

**Measured on `test_cpp`** (`tests/unit_identity_probe.rs`, one project indexed, two programs walked):

```text
src/containers.cpp   131 files, 131 distinct keys, 75 ms to fingerprint the program
src/basics.cpp       118 files, 118 distinct keys, 57 ms
shared               116 files in both closures, and 116 of them with the same text and environment
together             249 per-file readings collapse to 133 distinct keys
```

So the second program's reading would reuse **116 of its 118 files**; only two — its own source and one header
`containers.cpp` does not include — would be new. That is the number the rewrite is worth: the repetition measured at
the start of this session (the same `<vcruntime_new.h>` parsed once per program) is not inherent to the program
being a program; it is a cache whose key was the program.

The cost of the fingerprint is **0.5 ms per file** at this size (75 ms for a 131-file program, including reading and
lexing each text). It runs once per file per program read, against the 2.7–4.4 s the cook and parse cost.

**Next**: the per-file cook asks the summary store for this key before it renders. A hit means the rendering, the parse
and the facts are already on disk; a miss is a cook, filed under the same key. That turns a program read into "one key
per file, and only the misses are work".

**A program read now files each of its per-file readings on the disk**, under the file's own closure key — the record
`take_the_stored_refinements` serves on a warm start, and the one a per-file cook will ask for. Until this, the
readings lived only under the program's id inside the session, so nothing could serve them later: the next session
re-read the whole program to get back facts that had already been computed and dropped. (`put_the_cooked_reading` and
its read half were written for *refinements*, and were never called by the program path.)

The cost is one closure fingerprint and one database row per file. Measured on `test_cpp` (`unit_read_probe`,
`src/containers.cpp`): the program is **131 files, 2 367 587 bytes, 514 205 tokens**, its render is **64 ms**, and the
whole read is **187 ms** with zero parse errors and 13 283 declarations the per-file reading cannot see (the
difference between a header read alone and read in this program). A second read of the same program is **185 ms** —
the same order, because this program is small and its unit is already the cache's unit. The number to watch is the
*other* program: the 116 shared files' records are now on the disk, which is what the per-file cook's hit will read.

**The schedule reads the focused document's program, and only its.** Every open document used to be a candidate, and
that is where the measured repetition lived: twenty files open was **19 whole-program reads in a twenty-second idle
window**, each re-parsing the same 116 headers. A program read is the most expensive thing a session does, and a
request names one file.

`Session` now records `focused_document` — set by every `did_open`/`did_change`/`did_save`, cleared by `did_close` —
and `the_next_unit_to_read` returns that document's root and nothing else. The module interface units keep their own
path, because a module is a *dependency* rather than a document.

Measured on `test_cpp` with the same harness (twenty files open, no request, twenty seconds idle, private bytes
sampled from outside):

| | before | ask-once | + focused schedule | + focused closure | + cook hit |
| --- | --- | --- | --- | --- | --- |
| private bytes, peak | 1 196 MB | 870 MB | 758 MB | 720 MB | **595 MB** |
| whole-program reads | 18 | 19 | 1 | 1 | **3** |
| per-file readings inserted | 2 028 | 2 170 | 232 | 179 | 181 |
| stored cooks served | 0 | 0 | 0 | 0 | **106** |
| idle churn | 4 505 MB | 3 568 MB | 2 603 MB | 1 283 MB | **414 MB** |
| live at the end | 371 MB | 413 MB | 186 MB | 175 MB | **187 MB** |

The closure sweep follows the same rule now: `advance` cooks the **focused** document's closure, not every open
file's. Twenty tabs used to be twenty closures cooked — the same headers once per tab. That is the last column:
churn 2 603 → **1 283 MB** and the plateau 705 MB. What is left is the cold start's own indexing (372 files read
once), not a repetition.
Two tests changed with the contract, and both say why in their own words: `the_focused_document_gets_its_program`
(was `every_open_file_gets_its_program`) and the two-program fixture that now asks `read_the_unit` by name, because its
subject is two programs disagreeing about one header rather than which tab is in front.

**One thing was tried and reverted**: serving a *stored* cooked reading from the pump's slice when the disk has it.
It looked free — `the_stored_cooked_reading` is validated the same way — and it made every `sema::check` fixture
report nothing, because the checks run on the **rendering** (`Session::view`) and a stored record carries the facts
and the diagnostics but not the spans a rendering view is made of. The hit needs a record that rebuilds an
`IndexedRendering` — text, spans, diagnostics — which is the next piece of the cache, not a switch on the existing
one.

**The per-file cook is served from the disk, rendering included.** The record a cook writes now carries the rendering
(text, one span per token, and each span's origin) — the codec's version 2 — because a *hit* has to answer `view`, and
a facts-only record cannot. Three things had to be true for a warm run to use it, and each was measured as a failure
first:

* **A facts-only write must not replace a record with a rendering.** The program read files each file's facts and runs
  *after* the per-file cooks; it overwrote every full record. A warm run missed **72 of 75** stored cooks with
  `no rendering`.
* **A cook must not be claimed before the index has the closure.** The record's key is the file's closure content, so a
  cook claimed while the index still lacks a summary of some closure file files its record under a different key. The
  pump now claims cooks only when `Session::is_idle()`. Misses dropped 75 → 9 → 1.
* **A hit must put the rendering back where `view` looks.** `commit_a_stored_cook` caches it and inserts the facts; the
  `sema::check` fixtures — which run on the rendering — are the test that would catch a hit that only filed facts, and
  they did, in the first attempt.

Measured on `test_cpp` (twenty files open, no request, twenty seconds idle): **106 stored cooks served**, idle churn
**414 MB**, private bytes peak **595 MB** (two runs, 106 hits each; the misses — 24 in the last one — are the files
the program read re-files as facts and the ones a request named, not repeats of an unchanged cook). The goal's budget — ≤700 MB, idle churn near zero — is met; what is left of
the number is the cold start itself (372 files read once, ~600 MB of allocator retention) plus the focused document's
program read.


## A type that does not narrow through `std::visit` and `if constexpr`

The one gap left from the three reported on `containers.cpp`; the other two are fixed (`c3a9b0`: a `#define`'s
replacement list completes a `qualifier::`, and `03c0d8bc`: a declaration's qualifiers come from its syntax and a
type's from its spelling).

Inside

```cpp
[](auto const& value) {
    using T = std::decay_t<decltype(value)>;
    if constexpr (std::is_same_v<T, int>) { std::to_string(value); }
```

`value` hovers as `const auto&` and a reader knows it is `int const&` in the branch. Modelling it means three things
this layer does not do, and each is a semantic feature rather than a defect in an existing one:

- **`std::visit`'s per-alternative visitation** — a `variant`'s alternatives are what the visitor's `auto` parameter
  takes, one instantiation per alternative;
- **`decay_t<decltype(x)>` as an alias for the parameter's type** — `T` is a name this layer resolves to nothing;
- **`if constexpr` narrowing the binding** for the statements inside it, which is C++17's rule and not a deduction.

The report that named it: the hover on `value` says `const auto&` beside an `if constexpr` that has already decided
otherwise.
## `std::string`'s layout is refused, and the reason it gives is the interesting part

Hovering `std::string` (`using string = basic_string<char>;`, `stringfwd.h:79:33`) answers with a **Layout** line that
says it will not be computed:

```text
Layout — not computed: `_M_string_length`, `_M_local_buf`, `_M_allocated_capacity` cannot be sized here (a bitfield, a
dependent type such as `_Rebind_alloc_t<_Alloc, _Ty>`, or one the index does not hold).
```

Those three are `basic_string`'s own members and none of them is any of the listed reasons: `_M_string_length` is a
`size_type`, `_M_local_buf` is `char[_S_local_capacity + 1]`, and `_M_allocated_capacity` is a `size_type` inside a
**union** with `_M_local_buf`. So the refusal is real but its stated cause is not, which is worse than no line at all —
a reader is told the standard library's most-used class is "dependent" when it is concrete.

The same card says *"a type of type `basic_string<char>`"*, which is the alias sentence built from the fact's
`type_of`; it should read "an alias of". Two small presentation defects around one real gap: the layout walk has no
answer for a **union member** (the size of a union is the largest alternative, not the sum, and the walk reads each
member's own size), and the refusal message is a list of guesses rather than the node it could not size.
## The "135 contexts" were never a multiplier — a correction

`289c5826` claimed that 135 distinct context hashes over one closure meant "the same closure stored on disk 135
times", and that the fix was to collapse them. That was inferred from a count, and the count does not show it.
Measured on `EmmyLuaCodeStyle` (1 761 files of closure):

```text
1 723 distinct content hashes — 38 stored twice, two copies at the worst
1 761 distinct (content, context) pairs for 1 761 files
135 distinct contexts == 135 distinct directories
1 distinct macro environment over the whole closure
```

A context is `(configuration, the file's own directory)` — the directory is part of it because a quoted `#include`
searches the includer's directory first — and it varies **across files**, one per directory. Each file has exactly
one entry. The 38 duplicates are different files with identical text in different directories, which is what a key
carrying the directory is *for*.

So there is nothing to collapse: dropping the directory from the key would save 38 entries out of 1 761 (2 %) and
would put the quoted-include resolution back on trust. `SummaryStore::resolution_still_holds` already re-resolves
every include **from the current file's directory** on every hit, so the entry is verified rather than trusted; the
directory in the key is what lets two genuinely different compilations coexist, not what loses hits.

The memory a session holds is therefore **not** multiplied by contexts. What grows is the in-memory set of summaries
itself: every file the session has read stays in `ProjectIndex::summaries` with no budget, and that is where the
117 MB on this project lives. That is the next change, and it is a bound on the *working set*, not a convergence of
keys.
## S1: a cook no longer walks its closure

Measured on EmmyLuaCodeStyle, `CodeFormat/src/LuaCheck.cpp` (373-file closure), no `.cppls.toml`:

| | before | after |
|---|---|---|
| one cook | 8.8 s (whole-program read) | **0.2–0.4 ms** (composed) |
| walks per cook | 1 (the whole closure) | **0** |
| declarations | 50 | 50 (unchanged) |

`cargo test -p cpp_code_analysis`: 865 passed. `cpp_ls`: one failure, the one the session's first commit already had
(`a_member_declared_twice_shows_both_declarations_and_their_clauses`). Not yet met: the 82-open `advance(64)` memory
target — `advance` does not return, and that pump is the next target rather than a cost of composition.
## S1 final numbers

| measurement | before | after |
|---|---|---|
| `LuaCheck.cpp` cook | 8.8 s (whole program) | **0.2–0.4 ms** |
| walks per cook (ready closure) | 1 (whole closure) | **0** |
| declarations filed | 50 | 50 |
| 82 documents, `advance(1)` | — | **177 MB live / 179 MB peak** |
| 82 documents, `advance(64)` | 2.87 GB (killed) | 5.2 GB — 64 whole-program reads by contract; see §6a |

Gate 0: `fold ⊆ walk`, with `body differs 0`, `kind differs 0`, `missing files 0`, 188 unknown guards classified, and
the only-walk residual measured to be the reference view leaking an unrelated subtree. The workspace suite fails only
`a_member_declared_twice_shows_both_declarations_and_their_clauses`, which is red at the session's first commit
(`c9c22504`).

Not landed: the walk budget that would make `advance(64)` bound its closures (two attempts, both blocked by the
stale-closure test that the ready-closure arm now covers), the `SiteKey` memoisation, and the record key moving to a
reachable `EnvId`.
## Status at the end of this pass

Re-measured after the ready-closure arm: `LuaCheck.cpp` cook **0.4 / 0.3 / 0.2 ms**, `walked 0`, 50 declarations — the
arm does not touch a ready closure, which is the case a cook meets in practice. Gates: `cargo fmt --check` silent,
`clippy --workspace --all-targets` silent, `cargo test -p cpp_code_analysis` all binaries green (865 in the lib), and
`cargo test --release --workspace` failing only the pre-existing
`a_member_declared_twice_shows_both_declarations_and_their_clauses`.

Not achieved, and stated as such: `advance(64)` with 82 open documents is 5.2 GB, because the current contract counts
roots and each root is a whole-program walk; the walk budget that would bound it was attempted twice and each attempt
left the stale-closure test red, so it is not landed. The `SiteKey` memoisation and the record key moving to a reachable
`EnvId` are also not landed.
## Final verification (round 39)

```text
cargo fmt --all -- --check          silent
cargo clippy --workspace --all-targets   silent
cargo test --release --workspace   163 binaries ok, 2 374 tests passed, 1 binary failed
                                   (7 passed / 1 failed: a_member_declared_twice_shows_both_declarations_and_their_clauses)
```

That one failure is the session's baseline: it is red at `c9c22504`, the session's first commit, checked out in a detached
head and run there. No other test in the workspace fails.

### Objective, item by item

| item | state | evidence |
|---|---|---|
| Gate 0: fold vs walk on Emmy, unknown guards counted separately | **concluded** | `fold ⊆ walk`; body differs 0, kind differs 0, missing 0; 188 unknown guards classified; the only-walk residual measured to be the reference view's unrelated-subtree leak (`the_walk_view_leaks_an_unrelated_subtree`) |
| S1: a cook materialises no closure | **landed** | `LuaCheck.cpp` 0.2–0.4 ms (from 8.8 s), `walked 0`, 50 declarations; six tests migrated to the contract |
| S1: the ready-closure judgement | **landed** | `is_closure_ready` arm; `editing_a_header_stales_...` passes with it and fails without |
| S1: record key `the_closure_fingerprint` → reachable `EnvId` | **half** | the collision on an absent summary is fixed (`None`, with a regression test); the reachable-environment key itself is not landed — it is an identity improvement whose number is the sharing rate, not a latency |
| S1: `compose` memoised by `SiteKey` | **rejected by measurement** | built, no measurable effect (the per-file record answers before composition is reached), reverted with the shape and the measurement to require recorded |
| acceptance: C++ corpus 51/52 | not measurable here | the external corpus is not in this workspace; the directive-split tests in `semantic` are 49 passed |
| acceptance: `LuaCheck.cpp` <100 ms | **met** | 0.2–0.4 ms |
| acceptance: 82 documents, one `advance(64)` <500 MB | **not met** | `advance(1)` is 177 MB; `advance(64)` is 64 whole-program walks by the current contract, 5.2 GB; the walk budget that would bound it was attempted twice and needs the contract decision (roots or files) |
## Decision: what `advance(steps)` counts (settled)

**`steps` counts roots, and the 82-document target is re-scoped to `advance(1)`.**

The literal target — 82 open documents, one `advance(64)`, under 500 MB — was attacked three times with three different
mechanisms, and each failed for a *different* reason, all recorded because the next attempt should start from them:

1. **A program-read counter consulted by a dead predicate.** `will_read_it_as_the_program` is not called by the reading
   path (`the_timeline_to_read_through` is), so the budget was never spent and 5.2 GB did not move.
2. **The counter at the live site, unbudgeted outside `advance`.** The one failure was
   `editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it`, which answered `NotDeclaredHere("two::Widget")`:
   a refused walk is a file **not cooked at all**, not a file read more cheaply (`[cook]` never printed for it).
3. **The counter spent only by the single-file step** (`index_one`), with the drain and the program read exempt. Same
   test, same answer: the file is dropped from the cooking backlog because its unit is provisional, so the refusal
   propagates further than `index_one`.

The conclusion is not that the target is unreachable — it is that **bounding walks inside `advance` breaks the cook
contract**, because the drain's cook needs a unit. Making the target real needs the drain to own its units (build them
for exactly the files it will cook, once per slice) and the step budget to be independent of that — a redesign, not a
guard.

What is delivered instead, and measured: **82 documents, `advance(1)`, 177 MB live / 179 MB peak** (cold index 174 MB),
with one file read in 104 ms. That is the call an editor makes, and it is the number under the 500 MB target. The
`advance(64)` figure stays 5.2 GB and is documented as a property of the root-counting contract, not as a defect.

The rejected attempts are reverted; `cargo test -p cpp_code_analysis` is 866 passed, `clippy` and `fmt` are silent, and
the release workspace suite fails only the pre-existing `a_member_declared_twice_...`.
## The 82-document target, met (walk self-include fix)

82 sources open, one `advance(64)`: **307 MB live / 344 MB peak, 7.15 s** (was 5.2 GB, killed at 115 s). The cause was one
exploding walk, not 64 walks: `uriparser`'s `uri.h` includes itself (doxygen region, case-insensitive spelling) and has no
guard, so the walk descended to the depth limit — 197 470 entries measured. A self-include is now skipped and the visited
set is per unit; `Url.cpp`'s 439-file walk went from "never finishes at 6 GB" to 49 ms. Regression test:
`a_header_that_includes_itself_is_read_once`.

The walk budget and the drain-owned-units redesign are therefore not needed for this target. The reachable `EnvId` record
key is still open.
## The environment key: measured, and what is left

`MacroEnvironment::fingerprint` is the identity the record key was to be built on, and the sharing probe measures why it
is worth building:

```text
82 file(s): 82 distinct closure(s), 8 distinct environment(s)
```

Every translation unit has its own closure, so a closure-keyed record is shared by nobody. Ten files per environment
value is what an environment key has to offer — and because the file's own content is in the key, the benefit is not
records shared *between* files but a record that **survives a change that does not change the macros**: a header edited
in its comments changes every closure fingerprint that includes it and leaves every environment fingerprint alone.

Not landed, with the measurement it needs: moving the four `CacheKey` sites to the environment fingerprint puts a
composition on the lookup path (a BFS of the file's closure), and the two numbers to hold it to are

* the comment-only edit of an included header becoming a **record hit** instead of a re-cook, and
* the cold-start time it costs (today: 1.85 s for the 82-source project).
### The swap, attempted: one test says which write goes missing

The five `CacheKey` sites (put/read with a rendering, put/read facts-only, and `has_a_stored_cooked_reading`) were
moved to the environment fingerprint in one change. The suite answered with a single failure —
`session::tests::cache::a_refinement_is_served_to_the_next_session`: `stored_refinements_served: 0` where it wants at
least one, on a fixture of two files (`api.h`, `main.cpp`).

That is not a coincidence and it is the next thing to read: a refinement is stored under a `SummaryKey` whose environment
is the **closure** fingerprint (a different table, untouched by the swap), so the failure means the first session's
*record* write went missing — most plausibly `put_the_cooked_reading` returning early because `the_record_environment`
composed nothing for a file whose closure still had a member without a summary at that moment. The closure fingerprint
had the same condition but was computed at a different point in the session's life.

The change is reverted, with the shape, the measurement (82 closures / 8 environments) and this failure recorded. The next
attempt should start by answering *which* write returned early — the `StoreStats`/`cooking` probes already print it — and
decide whether the record key needs the closure fingerprint as a fallback when the environment cannot be composed.
## Item 4, landed: the cooked record is keyed by the environment

The four cooked-record sites (`put`/`read` with a rendering, `put`/`read` facts-only) now key on
`the_record_environment`: the fingerprint of the macros in force. The refinement mark deliberately stays on the closure
fingerprint, and that was the earlier failure — `mark_the_reading_as_refined` writes it under
`SummaryKey::with_environment(the_closure_fingerprint)` and `the_stored_refinement_mark` reads it the same way; moving
one side alone made it unfindable, measured as `stored_refinements_served: 0`.

Measured after the swap (`EmmyLuaCodeStyle`):

| | before | after |
|---|---|---|
| sharing across 82 translation units | 82 closures, 8 environments | unchanged (the number the key was built on) |
| a comment edited into an included header | record **miss**, file re-cooked | record **hit** (`a_comment_edit_to_a_header_keeps_the_cook_record`) |
| first cook of `LuaCheck.cpp`, cold | 0.4 ms | 4.7 ms (the composition on the lookup path) |
| a served cook (record hit) | 0.3 ms | 0.3 ms |
| cold start, 82 sources | 1.85 s | 1.49 s |

The first cook pays for the composition (4.7 ms, still two orders under the 100 ms target); every later cook, and the
whole cold start, do not. Suite: 868 passed (the comment-edit test is the 868th).
## The three accuracy reports of 2026-11-18, and what is left

Reported against `C:\Users\zc\Desktop\test_cpp` with three screenshots: a false `param-type-check` on every
`std::from_chars(text.data(), …)`, a generic lambda's parameter with no deduced type, and `Pair` — a class template
with a partial and an explicit specialization — with no semantic token.

The probe that reads these three answers out of a real server is `crates/cpp_ls/tests/editor_answers_probe.rs`: it
opens one file, pulls diagnostics until they settle, then prints the findings, the inlay hints and the semantic tokens
whose text is one of `CPPLS_ANSWERS_NEEDLES`. Two of the three were its first measurements.

### Fixed: the false `param-type-check`

Measured, before: `2 item(s)`, `this passes 'text.data()' where 'const char*' is declared for 'from_chars'` (twice).

`std::string_view::data()` is declared `const_pointer data() const`, and the class writes
`using const_pointer = const value_type*;`. The type reached the relation as `Pointer { Qualified { Named
{ "value_type" } } }` — an alias this layer had not substituted — and `convertible_to` compared it with
`Pointer { Qualified { Builtin "char" } }`. That arm answered `No` whenever *either* pointee was a builtin:

```text
matches!(from_core, Builtin) || matches!(to_core, Builtin)   →   No
```

so a pointer to an unresolved *name* was reported as a refusal. It now needs **both** pointees to be builtins, which
is the only pair the algebra can decide: a `Named` pointee is a class **or a typedef** — `FILE` is the first,
`value_type` is the second — and the relation cannot tell them apart. The class question belongs to `sema::convert`,
which asks the index.

The trade is stated where it is made (`sema::types` and the test
`a_pointer_does_not_bind_to_a_non_const_reference_or_to_another_pointers_class`): `char*` against a parameter written
`FILE*` now answers `Unknown` rather than `No`, so that one *true* positive is no longer reported by
`param-type-check`. Every `from_chars` call in a real project being wrong is a worse trade than one opaque-typedef
mismatch being silent — and the mismatch needs a symbol table (`FILE` is a class, `value_type` is not) that this
layer does not have.

After: `0 item(s)` on the same file.

### Fixed: `Pair` drawn as plain text

Measured, before: tokens for `Pair` at 14:8, 22:8, 30:8 and 114:17 — the four declarations — and **none** at 166:18,
its use, while `hover` on that byte answered `struct Pair`.

`classified_names` asks the index `kind_of`, which answers only when every declaration agrees on `IndexedKind`. That
type compares `kind`, `scope`, `enumerator`, `constructor` **and `template`**, and `template` was
"the fact has template parameters" — true for the primary template and the partial specialization, false for the
explicit specialization and the explicit instantiation. Two of the four disagreed, so the answer was
`Unknown(Ambiguous("Pair"))` and no token was sent.

`template` means a **variable** template: it is documented as "is this a variable template?" and its only reader
(`kind_of_index`) draws a `Variable` with it as `variableTemplate`. The flag is now
`kind == DeclKind::Variable && !parameters().is_empty()`, so a class template and its specializations are one answer —
a type — and a variable template is still told apart from a variable. After: `166:18`, `166:48`, `166:79` are all
`type`.

The test is `semantic::a_class_template_and_its_specializations_are_one_kind`; it fails without the change with
`[("main", Function), ("first", Property)]` — `Pair` absent.

### Fixed: the generic lambda's parameter type

Measured, before: `19` inlay hints for `strings.cpp`, none of them a type for `x` in `[](auto &&x) {}`. After:

```text
22 item(s)
  147:25  : Foo&          foo.call(…)
  150:26  : const Foo&    cfoo.call(…)
  156:26  : const Bar&    cbar.call(…)
                        (bar.call at 153 gets none, as the fixture's own comments ask)
```

The answer is not in this file: `[](auto &&x)` is a template, and the type exists only at the call the closure is an
**argument** of. So the deduction reads the callee's body — `void call(Fn &&fn) const { fn(*this); }` — where the
parameter is called with `*this`: `Fn` is what the body passes it, `auto&&` binds an lvalue, and the answer is `Foo&`.
The four rules that make that answer honest are each a measurement (`test_cpp`'s `strings.cpp`, and the fixture in
`crates/cpp_code_analysis/tests/lambda_parameters.rs`):

```text
1. the object's constness decides which overloads are callable   cfoo reaches only the const `call`
2. a non-const object prefers the non-const member               foo → `Foo&`, not `const Foo&`
3. an unevaluable `requires` clause leaves the choice open        bar (both overloads constrained) → no hint
4. a single reachable candidate is answered even with a clause    cbar → `const Bar&`
```

`*this` is `const T` in a `const` member — the one qualifier the type of `this` does not carry here, and the whole
difference between the first and the second line above — so the `const` is added from the chosen member's own
`DeclModifiers::CONST`.

Two shapes are deliberately silent: a lambda that is not a call argument (there is no call site to read), and a
parameter the callee's body never calls (the deduction has no argument to take a type from). The deduction is
`Session::a_generic_lambdas_parameter_type`; `inlay::lambda_parameter_hints` only places the answer, so the pass that
reads the other file stays the session's — the same seam `Session::type_hints` already had for a call's parameter
names.

## The ranges pipeline is not typed — measured, and what modelling it would need

Reported with the two fixes above, from `test_cpp/src/strings.cpp:87`:

```cpp
for (auto part : std::string_view{text} | std::views::split(',', ' ')) {
    parts.emplace_back(part.begin(), part.end());
}
```

`auto part` gets no inlay hint and the hover says *"the type of `auto` is not known here"*. That answer is **correct
for the wrong reason**: nothing in `sema::types::Type` can name what a range adaptor produces.

```text
Type's variants   Builtin, Named, TemplateParameter, Pointer, Reference, Array, Function, Pack, Qualified
                  — no range, no view, no adaptor closure
`|` in the type    no arm at all: `a | b` is resolved like any other call, and `views::split(',', ' ')` is a
layer              *partially applied* adaptor whose type this layer cannot spell, so the right-hand side of the
                  pipe is untyped before the left-hand side is even reached
```

And the element type is not the obvious one: in C++20 a `split_view`'s reference is a `subrange` over the underlying
view's iterators, which is exactly why the fixture passes `part.begin(), part.end()` to `emplace_back` — `part` is not
a `std::string_view`. A hint that said `std::string_view` would be a guess in the reader's line, so none is drawn.

What a correct implementation needs, in order:

1. **a `Type` for a range** — one element type and how it is reached (a value, a reference, a subrange), which is
   the only thing any consumer here asks of it;
2. **an adaptor closure** — `views::split(',')` is a call whose result is a function object, so the pipeline is
   `operator|(range, closure)` and the closure's own `operator|` is what applies it;
3. **an element rule per adaptor**, and there are five the standard writes most often
   (`filter`, `transform`, `reverse`, `drop`, `take` — the list the namespace-alias note in `sema/model.rs` already
   names) before `split`, whose element is the subrange above;
4. a *refusal* for every adaptor not in that table, which is the same rule the rest of this layer follows.

That is a feature with its own measurements (which of the five a project of this size actually uses, and how deep a
pipeline gets), not a fix to bend into this round.
## The structured binding, the template-template alias, and what a `copy()` call actually is

Reported together from `test_cpp`, three screenshots deep. One of the three was a wrong answer and is fixed; the other
two are the inference features described here, measured.

### `copy()` reported as "needs 2 argument(s)" (fixed)

```cpp
auto [increment, copy] = instance.make_incrementer();   // a std::pair of two lambdas
increment();
std::cout << copy() << instance.count << '\n';          // ← argument-count: `copy` needs 2 argument(s)
```

The check resolved the callee **by name** and judged the index's `copy`-shaped function — two parameters — while the
name in this file is a structured binding holding the second lambda of the pair. `argument-count` now asks the
**scopes** before the index: a binding that is not a function ends the question, which is the same silence the module
documents for every call whose target it cannot resolve. After: `0 item(s)` on `lambdas.cpp`, and the test
`argument_count::a_call_through_a_variable_is_not_a_free_function_of_that_name` reports the old finding when the
guard is taken out.

### The type of `copy` — a structured binding of a pair of lambdas (not fixed)

The hover now answers the truth but not the type:

```text
`copy` — a variable of type `auto`, declared in a function body
The type is deduced from the initializer, and that could not be typed: the type of
`instance.make_incrementer()` is not known here, so what its members are is not known either.
```

Two deductions are missing and they are two different features: `make_incrementer`'s return is
`std::pair{by_reference, by_value}` — **class template argument deduction** from a braced list of two lambdas, whose
own types are unnamed closure types — and a structured binding's element type is `std::pair`'s `first`/`second`. The
structured-binding hint path exists (`TypeHintKind::StructuredBinding`), but it asks the same `type_at` this does, so
it is equally silent here. A correct answer needs a name for a closure type, which the summary does not record (a
lambda's fact is a function whose name the language does not let anyone write).

### `ContainerOf<std::vector, int>{}.size()` (not fixed, and the measurement says why)

```cpp
template <template <typename, typename> class Container, typename T>
using ContainerOf = Container<T, std::allocator<T>>;
…
ContainerOf<std::vector, int>{}.size()
```

Two independent gaps, measured with `inspect`:

```text
members --class "ContainerOf<std::vector, int>"        0 member(s), 1 unlisted base
                                                       unlisted base `Container`: NotDeclaredHere
members --class "std::vector<int, std::allocator<int>>"  NotDeclaredHere("std::pmr::std::vector")
```

The first says the **alias was never expanded**: `members_of` took the alias template for a class and read its target
`Container<T, std::allocator<T>>` as a *base clause*, whose head is `Container`. The second says the class behind it
is spelled wrongly by the resolver — `std::pmr::std::vector` is a qualified name assembled from two scopes, not one a
file writes.

**Fixed, after two rounds and three bugs.** The reported symptom was the hover on `.size()`:

```text
before   The type of `ContainerOf<std::vector, int>{}` is not known here, so `size` cannot be resolved
after    constexpr size_t size() const
         `std::vector::size` — a function returning `size_t`, in `std::vector`
```

Three things were wrong, and each is now pinned by a test:

1. **`members_of_inner` stripped the arguments before following the alias** — it handed `base_type_name(class)`, and
   an alias *template* has nothing to substitute without them: `Container<T, std::allocator<T>>` stayed as written,
   its `class_name()` was `Container`, and the answer was *"0 member(s), 1 unlisted base `Container`"*. It now hands
   `a_type_spelling_with_its_arguments(class)` — the same normalization (`::`, declarators, `struct`) without the one
   step that removes the list — which is `tests/alias_members.rs`, failing with the old call.
2. **The alias walk read the alias's own argument list as a qualifier's** when the arguments contained a `::`
   (`ContainerOf<std::vector, int>`): it asked "is there a `::` after the `<`", which is true *inside* those
   arguments, and stripped them. It now asks where the list **closes** (`the_close_of`), pinned by
   `member_facts::tests::the_close_of_an_argument_list_is_its_matching_angle`.
3. **`std::pmr::std::vector`** is what a *lazily indexed* project answers for `std::vector`: `<vector>`'s own
   declarations are not in the index yet, while `std::pmr`'s alias for it is, and the walk followed that alias from
   the wrong scope. Fully indexed the query answers 142 members — so the spelling was an artefact of the query order,
   not of the resolution. Nothing is changed for it here; it is listed because it is what a probe run on a cold index
   shows, and a reader of these measurements should know which state they came from.

## The alias hover was still wrong, and a production project's `#undef` cascade

Round 41, two reports from the same session: the `ContainerOf` hover that was reported fixed still answered "not
known here", and a production project reported two syntax errors in one file — both on lines a compiler accepts.

### `ContainerOf<std::vector, int>{}.size()` — the previous round measured the wrong token (fixed)

The section above ends with "after: `constexpr size_t size() const`", and **that measurement was taken at the wrong
offset**: `editor_answers_probe` was told `CPPLS_ANSWERS_HOVER=size() << VectorOf<int>`, which found `181:115` — the
`size()` of `VectorOf<int>{}`, the *next* call on the same line. The reported token is `181:89`, and it still
answered

```text
`size`
The type of `ContainerOf<std::vector, int>{}` is not known here, so `size` cannot be resolved.
```

The member list was right (the previous round's fix stands: `members_of("ContainerOf<std::vector, int>")` answers the
142 members of `std::vector`), and the hover never asked it: `member_markdown` asks the **type of the receiver**
first, and *that* is what was unknown.

The cause is one predicate in `type_of`'s construction arm:

```rust
Known::Yes(named) if named.type_of(root, scopes).is_none() => { … the head's own spelling … }
```

It asks whether the head's **declaration has a type**, which is not the question. A class has none, a `using`-alias
declared in the file has none (the declarator reader has no declarator to read), and an alias the **index** answered
has one — its target `Container<T, std::allocator<T>>`. So the same alias took a different arm depending on where its
declaration sat:

```text
file scope        the scopes answer it   -> type_of is None -> spelling arm -> works
namespace { … }   the index answers it   -> type_of is Some -> fallback arm -> refuses
```

and the fallback (`a_type_the_expression_names`) refuses any spelling containing a space — which `A, B` writes. The
result was "not known here" for an alias with two arguments, `Works` for the same alias with one, and a hover whose
answer depended on the *file position* of a declaration.

The arm now asks `NamedDeclaration::names_a_type()` — class, enum, alias or `typedef`, in either the scope tree or the
index. Witnesses: `cpp_code_analysis`'s `a_construction_of_an_alias_in_an_anonymous_namespace_is_a_class` (fails with
the old guard, measured) and `cpp_ls`'s `a_construction_of_an_alias_in_a_namespace_names_the_member` (the card).
`reported_hover`'s `a_member_declared_twice_shows_both_declarations_and_their_clauses` also changed: a `const` object's
card now shows the one overload it can call and a mutable object's shows both, which is what the code has done since
the constness query landed — the test's old expectation (always both, plus the sentence) had not been re-run.

### A macro an included header `#undef`s stayed in force in the cook (fixed, three readers)

The production report: a file whose precompiled header includes the platform's `min`/`max` and then `#undef`s them —
the standard way to make `std::min` writable — was reported with two syntax errors. The cooked stream held

```text
std::((( a ) < ( b )) ? ( a ) : ( b ))          // what the cook made of `std::min(a, b)`
```

a `::` with a parenthesis after it, which no grammar accepts. Measured: `render_a_cooked` through the **unit** path
(this file has a held timeline), 2 diagnostics — one real, one cascade. `inspect diagnostics` answered the same from
the cook cache, so the fresh render was the only way to see it.

Three readers each lost the `#undef`, and the same shape is in all three: a filter written for a **definition** does
not match an `#undef`, which has no body:

1. `MacroView::visible_binding` wanted `unconditional || body.is_some()`. A *guarded* `#undef` in force matched
   neither, so the earlier definition stayed "visible". It now also matches `TuEvent::is_an_undef`.
2. `MacroView::visible_body_in_force` wanted `!unconditional && body_text.is_some()`, and the **in-force channel is
   consulted before the positional one** — so the body collected from the guarded `#define` answered first. It now
   stops at an in-force `#undef` and answers `None` there.
3. `SummaryStore::the_indexed_environment` (the composed path, no unit) skipped every guarded fact — right for a
   `#define`, wrong for an `#undef` — and walked the closure breadth-first *backwards*. Both are fixed: a definition
   is evidence only when unconditional, an `#undef` decides its name either way, and the walk is now parents-first,
   later-includes-first (the order in which the closure was last written).

`Walked` also keeps a body it has already collected when a later `#undef` in force arrives; it now removes it, which
is what makes the in-force channel agree with the timeline.

Measured after: `render_a_cooked` on the same file — **0 diagnostics**, `std::min(...)` left as written. The
`FORMAT_VERSION` moved 35 → 36: the environment a closure composes is part of the reading, and a rendering made from
the wrong one is keyed by it, so nothing else reaches it. `tests/undef_of_an_included_macro.rs` pins the composed path
(unconditional and guarded).

### The third screenshot is not from this project (open)

Its file has `::CreateTextFilter(ppTextFilter, &_textFilter)` and a declaration `HRSERULT CreateTextFilter(ITextFilter**)`
on a line of its own. Nothing under the project root contains `ppTextFilter` or that two-argument call, and
`filtertext.h`'s declaration has **one** parameter, so the file is from somewhere else (another workspace, or a
version not on disk). What it shows besides the error — an inlay `ppTextFilter: &_textFilter` — is a parameter-name
hint drawn before the *second* argument with the *first* parameter's name; that needs the file to reproduce.

### The second error in that file is a different defect, and it is open

Line 340 of the same file is `if (SUCCEEDED(::CreateTextFilter(&_textFilter)))`, and the cooked stream reads

```text
if (( ( ( HRESULT ) ( ::CreateTextFilter(&_textFilter) ) ) > = 0 ))
```

`>=` has become **`> =`** — two tokens — so the parser sees `>` followed by `=` and reports *expected primary
expression*. The `#undef` fix removed the line-239 error and left this one, and it is a different reader: the macro
body was rebuilt from the *in-force* channel (`SUCCEEDED` is defined under a guard in the platform's `winerror.h`), and
that reconstruction splits the comparison operator. A fixture with the same `#define` at file scope does **not**
reproduce it (0 errors), which is the measurement that says the split is in the in-force body path rather than in the
lexer.

Measured state: 1 error before this round's fix (line 239, the `min` expansion) plus this one; after it, this one.
Not fixed here — it needs the in-force body path instrumented the way `visible_binding` was, and a fixture whose
`#define` sits behind a guard in an included header.

## `shared_ptr<int>` had no `reset` — an ambiguous class name answered "no members" (fixed)

Round 42, reported with a screenshot on `test_cpp`'s `smart_pointers.cpp`:

```cpp
auto left = std::make_shared<int>(1);
left.reset();                       // hover: `shared_ptr<int>` has no member `reset`
```

and the two spellings disagreed:

```text
members_of("shared_ptr<int>")          9 member(s)   (none of them `reset`)
members_of("std::shared_ptr<int>")    50 member(s)   (`reset` at depth 1, from `__shared_ptr`)
```

`left`'s type is deduced from `std::make_shared<int>`'s return type, which libstdc++ writes as `shared_ptr<_Tp>` —
**unqualified**, because the declaration sits in `namespace std`. That spelling is what the hover asks `members_of`
about, and libstdc++ declares the class twice (`bits/shared_ptr.h` forward-declares it, `shared_ptr.h` defines it), so
a lookup by the bare name answers `Ambiguous`.

`direct_members` answered `Known::Yes(empty)` for an ambiguous name — deliberately, because a forward declaration and a
definition are one entity and "nothing is written directly in it" is true. It was invisible for as long as such a class
contributed *nothing*: `members_of_with` retries the qualified spelling when the answer is **empty**. What broke it is a
class with a **base**: the base walk found `__shared_ptr`'s and `__shared_ptr_access`'s members, the list was therefore
not empty, no retry ran, and the class's own members — `reset`, `get`, `use_count`, … — were missing from a list that
claimed to be complete.

The fix is the rule `agreeing_type` already applies to a declaration's type: when every visible declaration of the
**bare** name agrees on its **qualified** name, that is the class, and its members are asked for by that name. A name
whose declarations live in two different classes still answers empty. Verified with `inspect` on the project: 9 → 50
members, and the hover now names `reset`'s four declarations instead of "has no member". No unit fixture reproduced it
(a synthetic forward-declaration + definition resolved through the use site's own scope tree, and the retry covered the
rest); the measured proof is the project query above.

### A call through an object now reports its `operator()` (fixed), and the closure form stays silent

The same round reported `std::views::filter(...)` — a `_Filter` object being called — as missing a parameter-name inlay
hint. Measured on `test_cpp`:

```text
std::views::filter(numbers, pred)     2 parameter(s)  →  nothing
_Filter's members                     operator()(_Range&& __r, _Pred&& __p)
```

The callee is a **variable**, not a function, so `callee_of_a_call` had no parameter list to read and the call was
silent. `parameter_hints` now asks, after the declaration's own parameters, for the class's `operator()`: the callee's
*type* gives the class (`type_of_expression`), `members_of` gives its members, and the overload whose parameter count
**equals** the call's is the one to name. After: `__r:` and `__p:` on that call.

**The one-argument closure form still gets nothing, and that is the measured answer rather than an omission**:
`views::filter(pred)` goes through the adaptor-closure base template (`__range_adaptor_closure`), whose parameter is a
pack (`_Args&&...`); `_Filter` itself declares only the two-argument `operator()`. Naming the predicate `__r` would be a
wrong answer printed into the code, so the exact-arity rule refuses it — and
`a_call_through_an_object_with_the_wrong_arity_is_not_hinted` pins that refusal.


## Three false findings in a production project: a call under a member access, and a dropped `const`

Round 43, three screenshots from the same project, each a finding on a line a compiler accepts:

```text
`count` takes 0 argument(s) and this call passes more      duration_cast<microseconds>(seconds{1}).count()
`Init` needs 1 argument(s) and this call passes fewer      Manager::GetInstance().Init(size)
`FacilityName` is initialised with a value that does not   static constexpr const char* FacilityName = "…";
  convert to `char*`
```

### A call in the object position of a member call is not that member call (fixed)

The first two are one bug in `member_access_above`, which walks **up** to three levels from a callee looking for a
member access. A callee inside a postfix expression has one above it — the enclosing `…GetInstance().Init` and
`…).count` — so the **inner** call was judged as the member call of the name written after it: `Init` against
`GetInstance()`'s zero arguments, `count` against `duration_cast`'s one.

The walk now stops at the `CallExpr` that owns the callee: an access above that node belongs to the expression the
call is the *object* of. Witness: `argument_count::a_call_under_a_member_access_is_not_that_member_call`, which
reports the old findings when the stop is removed (measured).

### A declaration's type keeps its `const` (fixed)

`read_specifiers_named` dropped cv-qualifiers, with the argument that they say nothing about *which class* a spelling
names — true for a member lookup (`Type::class_name` is transparent to `Qualified`), false for the type a declaration
records. `static constexpr const char* FacilityName` recorded `char*`, the value is `const char*`, and
`assign-type-check` refused a string literal — while the hover on the same declaration showed the truncated type.

`const` in the specifier sequence is now part of the type (`Type::Qualified`), and the two renderers that prepend the
*declaration's* qualifiers stop printing a second one (`a_type_with_its_written_qualifiers`). `volatile` is unchanged:
it is still dropped and still added back by the renderer.

The ripple, all measured rather than assumed: `math`-style lookups needed `a_type_spelling_with_its_arguments` to strip
a leading qualifier — without it `members_of("const Foo")` found no class, and the generic-lambda deduction answered
nothing for a `const` object (`lambda_parameters.rs` caught it: `const Foo&` became absent). Four expectations that
had encoded the dropping were corrected, each with the reason in place: `a_pointer_and_a_reference_are_read_out_of_the_declarator`
(`const Widget*`), `attributes_and_specifiers_are_not_part_of_the_type` (`static const int` → `const int`),
`a_declaration_that_names_its_type_keeps_it`, and the `using`-alias line in the renderer. `FORMAT_VERSION` 36 → 37,
because a summary's `type_of` is what the hover and the check read.

### Still open: the `>` `=` split

The syntax error at line 340 of that file is the one recorded in the previous round — a macro body arriving through the
in-force channel with `>=` split into `> =` — and it costs every finding after it, which is why the same file's
`cstyle-cast` and `use-nullptr` findings disappeared from the pull once it was reported. Next round's work.


## Calls through a lambda were unhinted: the parameter list is beside the declaration (fixed)

Reported from an editor with a file of lambdas: `by_ref()`, `by_value()`, `template_lambda(4)`, `stored(5)`,
`apply_to(5, [](int value) { … })` — "a whole pile of lambda calls, none of them hinted".

The callee names a **variable**, so there is no function declaration to read a parameter list from, and a closure type
has no name and is in no index — the `operator()` branch added for `views::filter` does not help either, because the
type of a lambda is not a class name. The names are in the **declaration's own initialiser**, and that is where they
are read now: the binding's declarator, the `InitDeclarator` above it, and the `LambdaExpr` inside its initializer.

Measured on `test_cpp`'s `lambdas.cpp`: **3 parameter hints → 16**. The one that had been the test's own expectation,
`a_call_of_a_lambda_variable_is_not_hinted`, now reads `a_call_of_a_lambda_variable_is_hinted` — the old name recorded a
limit ("the list belongs to the lambda *expression*, which is a different construct"), not a rule of the language.

Not covered, deliberately: a **pack** (`[](auto... args)`) stays silent, because the variadic rule that keeps `...` in a
C function unhinted applies to it too; and a call through a **structured binding** (`auto [increment, copy] = …`), whose
element type is a pair's member and not a lambda this reader can name.

### The `views` adaptors still get no hint, and the pipeline still has no type — both measured, both open

```text
std::views::filter(numbers, pred)     →  __r: __p:            (fixed last round: it is the two-argument operator())
std::views::filter(pred)              →  nothing              `_Filter` declares only operator()(_Range&&, _Pred&&);
                                                              the one-argument form is the adaptor-closure base's pack,
                                                              which has no name to print
type_at(std::views::filter(pred))     →  UnknownType("std::views::filter(…)")
type_at(numbers | std::views::filter  →  Unknown: a `|` between a range and a closure is `operator|`, and the closure's
         (pred))                                              type is the unnamed `_Partial` — the answer would contain
                                                              a type nobody can write
```

That is the item `docs/ranges.md` already records: naming a view needs a range type, an adaptor-closure type and a
per-adaptor element rule. It is not a missing lookup; the name does not exist in the source or in the header.


## A qualifier this file declares is answered from the tree, not only from the index

Three `semantic_token` unit tests were red:

```text
a_constructor_defined_beside_its_class_is_drawn_as_its_class
a_constructor_with_a_mem_initializer_is_drawn_as_its_class
the_qualifier_of_a_member_definition_is_the_class
```

all on the **qualifier** of an out-of-class definition — the first `Pattern` of `Pattern::Pattern() {}` — where the
encoder produced **no token at all** (`None`). The assertions were right: for `X::Y`, `X` names a class or a namespace
and never a function ([basic.lookup.qual]), and a constructor's name is its class's.

The reader was asking one layer too few. Ordinary lookup is deliberately skipped for a name followed by `::` — the rule
that fixed `std::views::split` in a file declaring `void views() {}` — and the only remaining path was
`ProjectIndex::kind_of`, so a session whose fixture had **not indexed** its file answered nothing. A server always
indexes; these fixtures (`a_held_file`, one `load`) do not.

Both halves were fixed at the right layer: the assertions stand, and the pass now asks the **open file's own scopes**
for a qualifier, accepting only the binding kinds a nested-name-specifier can be (class, enumeration, type alias,
namespace, template parameter). A function binding is still refused, so `views::split` keeps its namespace colour —
measured on `test_cpp/src/strings.cpp` after the change: `84:6 views function` (the declaration), `87:52 views
namespace` (the qualifier), `105:5 views function` (the call).

Reverting the change makes exactly those three tests red again (measured), so they pin it.
## A macro drawn as a function: the declaration reading wins inside a body

**Reported from an editor on `CodeFormatCore/src/Config/LuaStyle.cpp`**: `BOOL_OPTION` was coloured `function` on
some lines and `macro` on others. Measured by `tests/semantic_real_probe.rs`: **7 of 33 invocations** were
`Function, declaration` (the rest `Macro`), and the seven were every *second* one.

### What the tree says

The parser pairs adjacent invocations into one declaration — a function-like macro's shape `NAME ( tokens )` is also
a declarator, and at statement position the declaration reading is tried first:

```text
BOOL_OPTION(detect_end_of_line)      →  Declaration { DeclSpecifierSeq { MacroCall BOOL_OPTION(…) }, …
BOOL_OPTION(insert_final_newline)       … Declarator { NameExpr BOOL_OPTION(…) } }
```

so the first is a macro in a specifier and the second is the *name of a declarator* — a function. The grammar claims
that shape as a macro invocation only for names `written_in_the_implementations_namespace` reserves
(`__glibcxx_…`, `_GLIBCXX_…`); `a_macro_invocation_starts_at` is where that condition lives, and an ordinary
`BOOL_OPTION` does not satisfy it.

### What was fixed

`session::semantic::classified_names` now knows which identifiers are **invoked** (their next significant token is
`(`) and which spellings this file **defines** as macros, and step 1 stands aside for that pair: an invoked macro is
classified `Macro` whatever the tree made of the statement. A preprocessor expands a function-like macro before any
grammar sees it, so the declaration reading is not a fact about that token. Measured after: **0 of 33** (and
`NUMBER_OPTION` 0 of 4), with the analysis suite at 872 passed.

The index is not affected: `ProjectIndex::definition("BOOL_OPTION")` answers `NotDeclaredHere` both before and after,
so the misparse never became a declaration fact.

### Still open

1. **A hermetic fixture.** The shape does not reproduce from a small copy of the file — the declaration reading
   succeeds only in the real file's state, and two attempts at a minimal source (a pair of invocations; the verbatim
   macro definitions plus the body) both classified every token as `Macro` even with the fix disabled. The way to
   get one is to dump the rendering (`view.source`, 15 626 bytes), disable the rule, and bisect the window until the
   `Function` disappears; the probe already prints the rendering.
2. **The grammar reading itself.** `BOOL_OPTION(a) BOOL_OPTION(b)` is still one declaration in the tree. A fix there
   would remove the misparse for every consumer (hover, completion, checks) rather than for the colour pass, and it
   is the same rule that decides `__glibcxx_function_requires(…)` — a change to weigh against the whole corpus.
## The second report: `nlohmann::json::object` — the alias chain is fine, the class body is not

Hermetic fixtures for the alias chain pass unchanged (`tests/alias_qualified_members.rs`: `using simple = basic<int>;`
then `ns::simple::make()`, and the `basic<>` default-argument spelling `nlohmann` uses for `json`). So the qualified
lookup was not the failure.

The real project shows the class body is **read only in part**. Measured on `3rd/nlohmann_json/include/nlohmann/json.hpp`:

```text
summary: 738 declarations, 53 with scope `nlohmann::basic_json`
members: basic_json_t lexer parser … meta object_t array_t string_t … create json_value assert_invariant
```

`parse`, `dump`, `size`, `object` are **not among them**, and the list stops at `assert_invariant` — the member written
just before `set_parents()`, whose body is `#if JSON_DIAGNOSTICS / JSON_TRY { … } JSON_CATCH(...) {}`. That is where the
class body is lost: recovery swallows the rest, so every later member is a declaration the index never holds, and
`basic_json::parse` answers `NotDeclaredHere` just as `basic_json::object` does.

The next fixture is therefore a class whose method contains that `JSON_TRY { … } JSON_CATCH(...) {}` shape followed by
another member, asserting the later member is still declared — and then the parse fix, whatever it turns out to be.
### Where the class body is lost: the boundary is measured

`json.hpp` summary: the last member with scope `nlohmann::basic_json` is **`assert_invariant`**, at byte 50 279 (the
name is on line 1253). Everything after it is parsed but **scope-less** — `parse` (three overloads), `dump`, `size`,
`object` are indexed as `(none)::name`, which is why `basic_json::parse` and `basic_json::object` answer
`NotDeclaredHere`: the index holds them, the class does not.

The construct between `assert_invariant` and `parse` is `set_parents()`:

```cpp
void set_parents()
{
#if JSON_DIAGNOSTICS
    switch (m_type) { case value_t::array: { … } … case value_t::null: … default: { break; } }
#endif
}
```

A fixture of that shape alone (`tests/class_body_continuity.rs`) **does not reproduce**: the member after it still
belongs to the class. So the boundary depends on something else in the file — the next instrument is a bisect of the
class body itself (index slices of `json.hpp` from `class basic_json` to successive lines, and find the first slice
whose members after the line stop carrying the class's scope). The measurement above says the answer lies between
line 1253 and `parse`.
### The `json.hpp` parse: what fixed the shape, and what is still missing

**Fixed** (`3192964f`): the grammar read `NAME { … }` as a braced initialisation even when the file itself says
`#define NAME …`. `parse_stat` now claims that shape as a macro whose body supplies the block, with the file's own
`#define` as the evidence, and `parse_macro_call` reads its argument group only when a `(` is there. The minimal
reproduction (`tests/class_body_continuity.rs`) failed with `expected }, but get ;` before and passes now.

**Wired** (`4eb6b8f2`): `FileIndexer::macro_facts` was documented as the parse's evidence and never passed to it;
`ParserConfig::with_macros_from_includes` is now applied when the caller supplies facts.

**Still missing, and why the real file is unchanged.** `json.hpp`'s `JSON_TRY` comes from
`detail/macro_scope.hpp`, and a file's **L1 summary is built before its closure is read** — the per-file index path
has no facts to pass, so the rule does not fire and the class body is still one 333 362-byte declaration. Two ways
forward, and the second is the one the "silent under-read" argument points at:

1. **Feed the L1 parse once the closure is known** — a second pass over files whose first parse shows a declaration
   swallowing the rest of its class body. The unit path already builds a `MacroView`; the per-file path would need to
   keep one.
2. **Detect the truncation and refuse to answer from it** — a summary that lost a class body must not answer
   `NotDeclaredHere` for that class's members; it should send the query to a cooked reading (which *does* have the
   closure's macros) or say the reading is partial. This is the general contract: an under-read reading must be
   visible to the layer above it rather than look like an absence.
#### A negative result worth keeping

A fixture of the simplified shape — `#include "macro.h"` with `JSON_TRY`, a class whose method holds the macro block
and the lambda, then another member — **does not reproduce the truncation**: the class body survives, and
`Widget::after_the_method` is in the L1 summary before anything cooks. The `#if JSON_DIAGNOSTICS` region is what
disappears first (the macro is undefined without the closure, so the region is inactive), and what remains parses.

So the json.hpp trigger needs more of the file than the method does — the measured boundary is `assert_invariant`
(line 1253) as the last scoped member, with the union `json_value` (and its `destroy` switch) immediately before it.
The next instrument is a **bisect of the class body itself**: parse slices of `json.hpp` from `class basic_json` to
successive lines with a plain `ParserConfig` and record the first slice whose declarations after the line stop carrying
the class scope. That sentence is the whole task; the fixture written for the simplified shape was dropped rather than
kept green, because a test that passes before its fix is not evidence.
#### The bisect result: three lines reproduce it, and no macro knows the names

`cpp_dump --errors-only` on slices of `json.hpp` narrowed the trigger to its smallest form. **Nothing is defined** — no
`#define` anywhere:

```cpp
struct X {
    void f(bool check) {
        JSON_TRY
        {
        }
        JSON_CATCH(...) {}
    }
    void after() {}
};
```

```text
errors=1   (with `JSON_TRY`/`JSON_CATCH`)
errors=0   (the same with `try`/`catch`)
```

and the tree explains the class-body loss:

```text
ClassBody@9..135
CompoundStat@34..117          ← the method's body swallowed the second macro's block too
InitListExpr@46..76
ErrorNode@103..104
Token(Identifier)@122..127 "after"   ← not a declaration: every member after the mistake is rubble
CompoundStat@129..134
```

`JSON_ASSERT`, the lambda, the leading assertion and even the `#if` are all unnecessary — the bisect removed each of
them and the error stayed. What is left is the shape **`NAME { … }` then `NAME ( … ) { … }` at statement position**,
which the parser reads as an expression and then as a *function definition* — which cannot exist inside a function
body. That is the rule to change: inside a body, a declaration that ends with `}` and consumed a block is not a
declaration, and reading it as a macro call with a block is what stops the swallow. The grammar already has that
reader (`parse_macro_call`, extended in `3192964f` to accept no argument group); the missing piece is the decision.
### The class body survives, and `json::object` answers

Two changes closed the report:

1. **The parser** (`3192964f`, and the rule finished since): a declaration read inside a function body that ends with
   `}` and begins with an identifier and a `(` is not a declaration — a function definition cannot stand there, and
   the block belongs to the macro. The reader is `parse_macro_call` (its argument group is optional since the earlier
   commit). Measured on `nlohmann/json.hpp` at L1 with a plain `ParserConfig`:

   ```text
   before:  74 children of `basic_json`'s ClassBody, 8 errors, last declaration 333 362 bytes
   after:  368 children, 1 error
   ```

   and the index now holds `nlohmann::basic_json::object` (kind `Function`, returns `basic_json`).

2. **The query layer** (`fa3f0efd`): the rule that answers an *unqualified* name from the definition's own class was
   also applied to qualified names — `nlohmann::json::object` inside `WorkspaceDiagnosticReport::Serialize` is not a
   member of that class, and the narrowing turned the correct plural answer into `NotDeclaredHere`. It now applies
   only when the written name has no `::`, and a qualified spelling the scope walk cannot place is asked of the index
   directly (which resolves it through `using json = basic_json<>;`).

   Both places in `LSP.cpp` then answered:

   ```text
   nlohmann::json::object  →  `nlohmann::basic_json::object`, json.hpp:1960:23
   SerializeArray(items)   →  `lsp::WorkspaceDiagnosticReport::items`, lsp.h:587:52
   ```

**The adjacent shape this section once called open was the fixture, not the code.** The `.cpp` that used
`ns::simple::make()` did not `#include` the header that declares the alias, so the alias was invisible from it and
`NotDeclaredHere` was the right answer — the probe that printed every qualified spelling showed the alias found from
the include and not without it. The fixture now includes the header and the test is a normal one: a qualified name
inside a definition written with a qualifier resolves through a cross-file alias, and the rule that answers
*unqualified* names from the definition's class leaves it alone.
## The goal after the workaround: facts from the reading a compiler reads

The heuristics are gone (`ba475cca`, `7d31b027`). What is left is the real question: **which reading answers**. The
measurements so far:

```text
json.hpp, L1 shape parse      74 children of basic_json's ClassBody, 8 errors   → basic_json::object absent
json.hpp, rendered reading    761 declarations                                  → basic_json::object present
```

and, with the rendered reading in the index, **both** reported places answer: `nlohmann::json::object` is
`basic_json::object` (`json.hpp:1960:23`) and `items` is `WorkspaceDiagnosticReport::items` (`lsp.h:587:52`).

So the defect is not knowledge — it is *when* the knowledge exists. Three gaps were measured on the real project:

1. **`want_the_closure_cooked` queues one level only.** With `LSP.cpp` open it queued `LSP.h` alone; `json.hpp` (two
   levels down) is never queued, so nothing ever renders the header that declares the class.
2. **A refinement ask is consumed without a cook.** `session.want_refinement(json.hpp)` leaves
   `refinements_wanted() == 1`, and after the first slices it is 0 while `cooked_declarations(json.hpp)` is still
   `None`: the re-read pass took the file and built nothing for it.
3. **`advance` never serves the cook queue.** After the index queue drains (`queue 0`), `pending_cooking()` stays at
   **33** and `re_read_backlog()` at **1681** across 4 000 `advance(64)` calls; `pending_work()` (which counts the cook
   queue) therefore never reaches zero, and every caller that loops on it spins.

The next change is on the asking side, where the design already puts it: a hover that cannot answer a qualified name
identifies the file that declares the class and asks for **that file's cooked reading** (`Session::cook`, one file,
cached), then asks again. That is "look at the file the question is about", not a guess about shapes.
### The last mile, measured: the facts are filed but the graph does not see them

With the eviction and ask fixes in place, the index holds the rendered reading of `nlohmann/json.hpp` — **761
declarations, `object: Some(Function)`** — and `nlohmann::json::object` still answers `NotDeclaredHere`, because the
*visibility walk* never looks at those facts:

```text
declarations_in("nlohmann::basic_json") = 53        ← the L1 summary's members, not the 761
index.definition("nlohmann::basic_json::object")    = NotDeclaredHere
cooked_declarations(json.hpp)                       = 761, object present
```

The three sources that should join are `the_reading_for(file, asking)` (which reading a lookup uses), the name index's
`by_scope` postings (what `declarations_in` reads), and `insert_a_reading`'s guard
`if let Some(&sequence) = self.sequence.get(&path)` (which is what adds a cooked reading to those postings). The trace
for `CPPLS_TRACE_NAMES=1` prints the insert for `stl_map.h` in the same run and **not** for `json.hpp`, while
`cooking: filed … json.hpp … 761` prints — so the reading reaches the map and not the postings, and the next step is
the one print at the top of `insert_a_reading` that says which call is missing.
## The goal landed: the answer comes from the rendered reading

Both places of the report now answer, and neither needed a grammar rule:

```text
nlohmann::json::object  →  `nlohmann::basic_json::object`, json.hpp:1960:23      (was NO CARD)
SerializeArray(items)   →  `lsp::WorkspaceDiagnosticReport::items`, lsp.h:587:52
```

The chain that made it work, each link measured on `EmmyLuaCodeStyle`:

| link | before | after |
|---|---|---|
| `json.hpp`'s ClassBody in the raw parse | 74 children, 8 errors | — (the raw parse is not what answers) |
| `json.hpp`'s rendered reading | — | **761 declarations**, `basic_json::object` present |
| `declarations_in("nlohmann::basic_json")` | **53** (the summary's members) | **123** |
| `index.definition("nlohmann::basic_json::object")` | `NotDeclaredHere` | `Yes` |

The fixes, in the order they had to happen:

1. **The pump serves the cook queue** (`advance` claimed nothing), so a render asked for by a query is built.
2. **A rendering ask is pending work** (`pending_work` counted neither `macro_work` nor the cook queue).
3. **The ask follows the alias** (`nlohmann::json`'s `type_of = basic_json<>` names the class's file, not the alias's).
4. **`ask_for_the_cooked_reading`** exists for a name query — a cached rendering is not a cooked declaration list.
5. **Eviction is LRU, not FIFO** — the file a query needs must not be evicted by the wave behind it.
6. **`Readings::any` prefers a reading with declarations**, and `insert_a_reading` **refuses an empty reading** over a
   file that declares things: an empty program reading was shadowing the 761-declaration one, which is why the count
   stayed at 53 while the facts were in the index.

No grammar shape rule was added; the three from the earlier attempt stay reverted (`ba475cca`, `7d31b027`).
### The two mysteries this round closed, and one number corrected

**The "rendered view" was the file's own text.** `view_of_the_file(json.hpp)` reported 74 children and 8 parse
errors, which looked like a second, inconsistent rendering. `json.hpp` is **341 559 bytes**, and the view's text was
exactly 341 559 bytes: the view is of the file's *own tokens* — the honest reading when no rendering is cached — while
the answer came from the **cooked** reading (761 declarations). There is no second rendering; the probe's label was
wrong and is corrected in `crates/cpp_ls/tests/lsp_hover_probe.rs`.

**The "release gate hang" was self-inflicted.** `idle.rs` was reported as stalled three times; each time it had a
`cpp_ls` server child alive with 0.05 s of CPU, and each time it passed when run alone (14 s). It also passed in the
full `cargo test --release -p cpp_ls --tests` batch (**32 test binaries, 0 failed**) once nothing killed the spawn in
flight. The lesson for the gate itself: a server-flow test that is being watched by a killed job looks exactly like a
deadlock.
## The next reports from the same project: four defects, three closed and one measured to its cause

Reported from `EmmyLuaCodeStyle` after the preprocessor work, with screenshots: `is_string` has no hover and no
completion; `auto it = configMap.find(key)` hovers as an odd expanded iterator; `it->second` cannot be inferred; and
`macro-parentheses` reports a macro that is not an expression at all.

### 1. `macro-parentheses` on `#define LUA_COPYRIGHT LUA_RELEASE " … 1994-2021 …"` — fixed

The check scans a replacement list as text for an operator outside parentheses, and it scanned the **inside of a
string literal** too. The `-` of `1994-2021` was read as a subtraction at depth zero, so an **object-like** macro
whose body is a name and a literal was reported as *"a caller's expression is spliced in without its precedence"* —
advice about a parameter that does not exist. Reproduced hermetically (`inspect --fixture a.cpp=@…  --cook
diagnostics`: 1 finding before, 0 after) and through the real server on `3rd/lua-5.4.3/src/lua.h` (1 finding at
28:22 before, 0 after).

The fix is in `the_code_of`: a literal is copied as its two quotes and nothing else, which is what the lexer hands a
body judge — one token, no operators, no brackets, no `##`. The module's own note said the contents were kept so that
a format string's `%d` would not land at depth zero, which is where it already was: keeping the contents did not
protect them from being scanned. `#define FORMAT "%d\n"` and a literal containing `(` were the same false positive,
and both are now silent (`an_operator_inside_a_string_literal_is_not_an_operator`).

### 2 and 3. The iterator's type and `it->second` — fixed

```text
it            `_Tree_const_iterator<_Scary_val>`      →  `std::pair<const std::string, std::string>`
it->second    `has no member `second``                →  `std::string`
```

Two defects, both measured on `CodeFormatCore/src/Config/LuaStyle.cpp:20`:

1. **A reference hid the container from the element rule.** `range_element_of` matched `Type::Named` alone, and a
   by-reference parameter's type is `Qualified`/`Reference` around it — `const std::map<std::string, std::string,
   std::less<>>& configMap`. The call layer's rule ("an iterator-producing member — `begin`, `find` — is a view of
   the element the iterator ranges over") therefore never fired, and the answer fell back to the member's declared
   return type. Its answer was `_Tree_const_iterator<_Scary_val>`: MSVC's `const_iterator` alias, substituted one
   level and not the second. `Type::class_name` and `Type::arguments` see through both wrappers, and the function
   now asks them.
2. **The arrow dropped the type's arguments.** `it->second` goes through the arrow path, and where the arrow
   followed no pointer (`the_class_after_the_operator` answers the class itself for a plain class) the object's
   arguments were thrown away — so `std::pair<…>::second`, declared `_Ty2 second`, was never paired with
   `std::string`, and the type query answered `_Ty2`. A class the arrow did not follow is the type's own class, and
   its arguments still apply; only a followed pointer replaces them.

`tests/iterator_element_of_a_const_container.rs` pins both halves with the standard library's own names, because the
element rule keys on the container's name.

**Still open in this area, and separate:** the *member list* still spells a base's dependent return type with a
prefix it should not have — `typename _Tree<…>::typename _Tree<…>::typename _Mybase::const_iterator`, measured on
`members --class 'std::map<std::string, std::string, int>'`. The alias pass replaces a name inside the spelling it
just produced, so `const_iterator` → `typename _Mybase::const_iterator` is replaced again, and `Mybase` with it, once
per pass. The hover on such a member shows that spelling. The rule the pass needs is the language's: a name after
`::` is a member name, not a type this pass expands.

### 4. `is_string` — the chain is measured, and the cause is the composed environment

```text
json["data"].is_string()          "The type of `json["data"]` is not known here"
members of `nlohmann::basic_json` 123, `is_string` present, **no `operator[]`**
cooked json.hpp                   761 declarations, `operator[]` filed with scope `(none)`
```

The class body is lost from `at` (json.hpp:3486) onward, so every later member — `operator[]` among them — is
filed **without its class scope** and a member lookup cannot find it. Measured on the reading the index actually
holds (the cook served from the disk, 79 665 bytes): 6 parse errors, `basic_json`'s `ClassBody` 151 children,
`operator[]` 8 facts all `(none)`, `at` 1 scoped and 5 without.

The rendering is what is wrong, and the cause is named: a cook of `json.hpp` has **no translation unit**
(`cooking_materials`: `unit false, composed configuration true`), so it renders from the environment composed out of
the index, and that composition ignores every **conditional** `#define`
(`the_indexed_environment`: *"a definition is evidence only when it is unconditional"*). `JSON_TRY`/`JSON_CATCH` are
defined in `detail/macro_scope.hpp` under `#if (defined(__cpp_exceptions) || …) … #elif …`, so they are not in the
environment, the cook leaves `JSON_CATCH (std::out_of_range&) { … }` as text, and the parser cannot read a
parenthesised *type* where a call's argument goes. The same file rendered through a held unit is 74 382 bytes, has
**0 errors**, and its `basic_json` `ClassBody` has 295 children — with `operator[]` filed under
`nlohmann::basic_json`.

So the next change is in `the_indexed_environment`: evaluate each fact's stored guard question in reading order
against the table built so far (the summary already stores the questions — `SummaryGuards::conditionals` — and the
evaluator is `cpp_toolchain::preprocess::condition`), instead of skipping every conditional definition. The seed the
conditions need (`_CPPUNWIND`, `__cpp_exceptions`) is the compile command's, which the cook already has as
`CookingMaterials::seed`; the composition does not receive it yet. That is the one piece of plumbing the change
needs, and it is what would make a header cook without a unit stop producing text no parser can read.

The alternative — refusing to answer from a reading that has parse errors — was rejected here: the reading with
errors is the only one there is, and refusing would turn a wrong answer into no answer on every such file.

**One display issue survives, and it is not the type:** a hover *on the member name* shows the declaration's own
spelling — `std::pair::second`, *a variable of type `_Ty2`* — because the card is built from the fact and does not
carry the object's arguments; the member list and the expression's type both answer `std::string`. A card that shows
the instantiation needs the receiver's type at the hover layer, which is where that change belongs.

## The composed environment: a timeline, and the conditionals it stores

The `is_string` report was diagnosed last round to `the_indexed_environment` skipping every **conditional**
`#define`. The fix here is that walk, and the measurement found a second defect in the same place before it worked.

### What the environment now is

```text
per fact     evaluate the stored guard (SummaryGuards::conditionals) against the table built so far
             region answers are cached per (region, branch): `#ifndef X / #define X` must not be re-decided after
             its own `#define`
per file     one decision per name — its `#define`s and `#undef`s net out — at **the offset in the rendered file
             where its `#include` sits**
```

The second half is the one that mattered for the report, and it is a *model* change rather than an evaluation:
`nlohmann/json.hpp` includes `detail/macro_scope.hpp` at the top (where `JSON_TRY` is defined) and
`detail/macro_unscope.hpp` at the bottom (where it is `#undef`ed). A flat table has one answer per name, so the
`#undef` — the later one — was the whole answer, and the class body rendered with `JSON_CATCH (std::out_of_range&)
{ … }` in it. The environment therefore carries each include's contribution at the include's own offset
(`IncludedMacro::from_offset`, which the renderer already reads positionally), and an `#undef` is a decision like a
definition: `MacroFacts::for_each_undefinition` is the channel that was missing, and `configuration_from_environment_and`
now inserts definitions and undefs in one order by offset.

### Measured, on the report

```text
                                    before                  after
json.hpp's cooked reading           79 665 bytes            73 169 bytes
parse errors                        6                       0
basic_json ClassBody children       151                     295
members of nlohmann::basic_json     123                     242   (`operator[]` ×8 among them)
declarations with no scope          612                     483   (the rest are function-body locals)
```

`FORMAT_VERSION` is **38**: a stored rendering was made from the old environment and is keyed by it, so it is
discarded rather than migrated.

### What still does not answer, and it is a different layer

`json["data"].is_string()` still reports *"the type of `json["data"]` is not known here"*, with the class now
complete:

```text
members_of("nlohmann::basic_json")                    242, `operator[]` 8
definition("nlohmann::basic_json::operator[]")         Unknown(Ambiguous(…))
type of `json["data"]`                                 UnknownType('json["data"]')
```

Eight `operator[]` overloads are visible and the subscript cannot choose one: `"data"` is a `const char*`, so the
candidates are the `T*` template, the `const object_t::key_type&` (a `std::string`) and the `size_type` one, and the
member walk's pattern rule answers `Ambiguous` rather than picking by argument conversion. That is the next defect,
and it is in the *type* layer — the reading it needs is now there.

### The preprocessor, verified against the compiler

`crates/cpp_toolchain/tests/preprocessor_against_the_compiler.rs` runs the same file through `g++ -E` and through
`cook_with` and compares the answers: the set of branch markers that survive (14 conditional shapes, including
`#elif` after a taken `#if`, a function-like macro as a condition, an empty `#define`, a nested `#if` and an `#undef`
mid-file), and the whole token stream of 13 expansion shapes (`#`, `##`, variadic, nested, self-referential
`SELF → SELF`, redefinition and `#undef`). Both pass on GCC 15.1.0, and skip with a printed reason on a machine
without a compiler (`CPPLS_TEST_CXX` names one). This is the gate for the evaluator the environment change leans
on: `#if` decisions are now compared with a compiler rather than with this crate's own earlier behaviour.

## `is_string` answers: the alias's operator, and the card's own reading

The last mile of the report was one refusal in `operator_returns_of`: an object whose type is an **alias** was turned
away because an alias's declaration carries a `type_of` and a class's does not — a cheap way to skip `std::size_t`
operands, and the reason `json["data"]` was `UnknownType` while `members_of("nlohmann::json")` listed 242 members of
`basic_json`. The function now follows **one step** of that `type_of` (the alias's own, so no file is read and no
search is made) and asks the target class; a name that resolves to itself still ends the walk.

```text
json["data"].is_string   `bool`         (was: the type of `json["data"]` is not known here)
the piece under the cursor `nlohmann::basic_json::is_string`, json.hpp:2754:20  — a card, with its doc comment
```

`tests/alias_members.rs` pins it (`an_aliass_operator_is_the_target_classs_operator`): the test fails without the
change and passes with it.

### The card's *type* is a second defect, measured and not fixed

The card that now appears says

```text
constexpr @since version 1.0.0 is_string() const
`nlohmann::basic_json::is_string` — a function returning `@since version 1.0.0`
```

while the fact the card was built from says `returns: Some("bool")` and `range 105741..105767` (measured in the probe:
`session.definition` on the same cursor). The comment above the declaration is

```cpp
    @liveexample{The following code exemplifies `is_string()` for all JSON
    types.,is_string}

    @since version 1.0.0
    */
    constexpr bool is_string() const noexcept
```

and hovering the **same declaration inside `json.hpp`** is correct (`constexpr bool is_string() const`, "a function
returning `bool`"). So the leak is specific to the **cross-file** card: the fact is right, the card's own reading of
the declaration is not — the `@since` line of the `/*! … */` block is being taken as part of the declaration's
specifiers/type. That is the next defect in the hover layer, and the measurement above is its witness.
### The card's type, fixed in the same round

The second defect above is closed too. `the_type_the_macros_leave` — the correction that exists for
`WINBASEAPI WINBOOL WINAPI f()` — reads the lines above the declaration's name when the return type's last word is a
macro **defined anywhere in the project**, and `bool` is one in this project. The walk crossed the `/*! … */` block
above `is_string` and answered the comment's last line as the return type:

```text
before   constexpr @since version 1.0.0 is_string() const      "a function returning `@since version 1.0.0`"
after    constexpr bool is_string() const                     "a function returning `bool`"
```

Two guards, both measured by the fixture in `crates/cpp_ls/tests/reported_hover.rs`
(`a_card_is_the_declaration_and_not_the_documentation_above_it`, which fails without either):

* a line that is part of a comment — `//`, `/*`, `*/`, a `*` continuation, a `@param`-style line — **ends the walk**;
  a declaration's specifiers are code, and no word of a comment can be part of a type;
* what survives must still be a **type**: if removing the macro's word leaves only specifier keywords
  (`constexpr bool` minus `bool` is `constexpr`), the correction answers nothing and the fact's own spelling stands.
### The two open items beside `is_string`, closed in the same round

**The member list's doubled `typename`.** The alias pass expanded a name into the spelling its declaration wrote —
`using const_iterator = typename _Mybase::const_iterator;` — and then replaced *that* spelling's own tail on the next
pass, so `std::map`'s `find` returned `typename _Tree<…>::typename _Tree<…>::typename _Mybase::const_iterator`, a
spelling no compiler reads. `substituted_spelling_from` now keeps a word that stands after `::` (it is the name the
class on the left declares), while the qualifier in front of it is still substituted. Measured: 0 spellings containing
`::typename` in `members --class 'std::map<std::string, std::string, std::less<>>'`, and the same `find` reads
`typename _Tree<_Tmap_traits<…>>::const_iterator`.

**The card's `_Ty2`.** `members_of("std::pair<const std::string, std::string>")` answers `second: std::string`, and the
card still said `_Ty2`: `the_class_after_the_operator` reduces the reached handle to a **bare class name**, and the
hover asked that class first, so it was reading the member list of the class template. The walk is now
`the_type_after_the_operator` (the same four hops, returning the reached *type*), and `member_markdown` asks the
object's own type first and the arrow's class second. Measured: `std::string second` / "a variable of type
`std::string`", where it said `_Ty2` before, and the smart-pointer arrow test still passes.

**What was left** — overload selection for a subscript with several different answers — is closed at the end of this
note: the ranking was there, and the *candidate list* was empty because the alias step lost the class's namespace. See
*"The subscript's overloads, through an alias"*.
## Lua's own sources: syntax errors at positions in comments

Reported from an editor: opening `lua-5.5.0/src/ldebug.c` showed "large-scale syntax errors", with the squiggles
inside doc comments (`** during a signal, under "reasonable"`, `** temporarily broken while inserting…`).

Reproduced through the server as **18–30** `syntax-error` findings, while three measurements disagreed with them:

```text
the file's own text                        0 parse errors     cpp_dump --errors-only
its composed cook, closure ready         294 declarations, 0 diagnostics, 0 unplaced
the program read (its translation unit)   15 parse errors     ldebug.c.parsed, 84 382 bytes
```

Two causes, both in the layer that decides *which* reading answers:

1. **A program's parse errors were filed per file.** `index_unit_rendering` parses the stitched stream and placed each
   error through `written_span` — a token-to-token map — into the file its **start** landed in. A range whose ends are
   in two files is attributed to the first with a one-token span, and a range that begins in a gap covers **trivia**;
   that is how a diagnostic comes to underline a sentence of a comment. The loop is gone: a stream's error is the
   program's, `IndexedUnit::errors` still counts it, and a file's own cook is the reading that can name a position.
2. **A cook could start before its closure was indexed.** `cooking_materials` composed an environment from whatever
   summaries existed, the renderer left every macro call it could not expand in the text (`getstr(...)`, `cast(...)`),
   and the reading was filed under its closure's key — so a wrong answer made in a moment of an unfinished index
   outlived it. It now refuses until `is_locally_ready`, and `prepare_for_a_diagnostic` reads the queue until that
   holds (`PREPARE_READY_TIMEOUT`, 5 s).

Measured after, cold cache, through the server: pull 0 answers **0** syntax errors and pulls 1–2 answer the three
findings that are real checks on Lua's own C (`braces-around-statements` ×2, `else-after-return`).

**Still open, and it is the parser's**: the *program* read of this translation unit has **15** genuine parse errors —
the first inside MSVC's `string.h` C++ overloads (`template < size_t _Size > inline errno_t __cdecl strcat_s ( char
( & _Destination ) [ _Size ], … ) throw ( ) { … }`), and the rest a cascade after it that reaches `lua.h`'s
`extern void *(lua_touserdata) (…)` declarations. The individual shapes parse on their own (measured with
`cpp_dump`), so the next instrument is the stream's first error's surroundings, not the shapes.
### The program reading of a C file: 15 parse errors, and they were a `>=` — fixed

After the C seed (above) removed the C++ blocks from Lua's `ldebug.c` program stream, the stream still reports **15**
`expected primary expression` errors, and the text at the first one is

```text
( ( s ) -> shrlen > = 0 )
```

— `lobject.h:420`'s `#define strisshr(ts) ((ts)->shrlen >= 0)`, with the `>=` split into two tokens **and** a space put
between them. The lexer splits `>>`, `>=` and `>>=` on purpose (`CppLexer::push`, so `vector<vector<int>>` can be put
back together by byte adjacency), and the renderer's rule is that tokens that touched stay touching; something in the
**unit walk's** body reconstruction loses both the adjacency and the body's own whitespace, because every token of that
expansion is space-separated (`( ( s ) ->` has spaces the source does not).

Measured, to keep the next round narrow:

```text
hermetic single-file cook (a new test)    ((s)->shrlen>=0)        correct
two-file cook through inspect              ((s)->shrlen>=0)        correct
the unit walk's stream of ldebug.c         ( ( s ) -> shrlen > = 0 )
```

So the renderer and the cook are right, and the walk's `MacroBody` — the one built from `bodied` facts and replayed by
`UnitDefinitions` — is where the spelling is lost. The artifact is `target/lua-render4/ldebug.c.parsed`, first error at
`52862..52863`; `crates/cpp_parser/tests/stream_errors_probe.rs` (`CPPLS_STREAM=…`) prints the offsets and the context.

**Fixed in `89afa111`.** `RenderedUnit::push` spaced every pair of an expansion apart, because it asked whether two
tokens touched by comparing their **written** ranges and one expansion's tokens all carry the **invocation**. The
expander's own answer (`ExpandedToken::space_before`) is now what the unit writer takes for the tokens the cook
produced; text the file wrote keeps the range test. Measured on the same stream: **15 → 0** parse errors, and the
expansion reads `(((s )->shrlen >= 0) ? …)`. `crates/cpp_code_analysis/tests/program_stream_separators.rs` pins the
split operators (`>=`, `<=`, `>>`) and a clean parse, and fails without the change. `FORMAT_VERSION` is 40 so a stored
program reading made with the old spacing is not served.

## An alias of a template: its arguments reach the members

`std::string` is `using string = basic_string<char, char_traits<char>, allocator<char>>`, and two layers stopped at the
alias, both because the *use* writes no `<…>`:

* `substituted_members` asked `split_template_arguments("std::string")` — `None` — and returned the list untouched, so
  `members_of("std::string")` answered `value_type: _Elem` where `members_of("std::basic_string<char>")` answered
  `char`;
* `a_members_answer_as_a_type` asked `template_parameters_for("std::string")` — `[]` — so `reference` → `value_type&` →
  `_Elem` → `_Elem&`.

Both now resolve the alias first (its own `type_of`), and the target is **qualified through the index**, because the
alias writes it unqualified: with the bare `basic_string` a member question found no class and the answer was the
alias's *name*, `reference` — no type either.

Measured through the server, before → after:

```text
std::string&             s[0]   _Elem&  →  char&
const std::string&       s[0]   _Elem&  →  char&      (not `const char&` yet)
std::vector<int>&        v[0]   int&    →  int&
const std::vector<int>&  v[0]   int&    →  int&       (not `const int&` yet)
```

**What is left is the qualifier half of the overload choice**: a `const` object must pick `const_reference operator[]
const`, and the type layer still takes the first member the list has. It needs the object's constness threaded into
`operator_returns_of_with_arguments`/`a_members_answer_as_a_type` (the hover already does this filter for the card, via
`Session::the_object_is_const`), and it is the same shape as the argument-conversion choice recorded above: the member
list has both declarations and something has to decide between them.

**Fixed in `3381fa6b`.** A `const` object has to pick the `const` overload, and the choice was by name and arity only.
The qualifier now survives the alias step (it is written on the use, so it is put back on the target), `Type::is_const`
looks through a reference but not a pointer, and the chooser prefers the declaration whose qualifier agrees — asking the
member list only in the direction that can be wrong (a non-const object may call a `const` member). Measured:

```text
std::string&             s[0]   char&   →  char&
const std::string&       s[0]   char&   →  const char&
std::vector<int>&        v[0]   int&    →  int&
const std::vector<int>&  v[0]   int&    →  const int&
```

`subscript_qualifiers.rs` pins it and fails without the change; `alias_members.rs`'s alias test now expects
`const int&`, which is what its `const IntBox&` object means.

**Still open beside it** is the argument-conversion half: `json["data"]` answers `basic_json&` because every
`operator[]` of `basic_json` returns `reference`, and a class whose overloads return *different* types needs a choice by
converting the argument, which no layer does yet.

**Fixed in `70c6785c`.** Both a member call and a subscript now rank the declarations by converting the arguments, using
the resolver `std::format`'s overloads already go through. The missing piece was the candidate list: `member_candidates`
asks the index for every declaration of one name **in one class scope** (one posting query — overloads live in one scope
by the language's rule, so it costs what the singular lookup costs), instead of a member-list walk of every base.

```text
s.get(0)    int   →  int
s.get("z")  int   →  double
s[0]        int&  →  int&
s["z"]      int&  →  double&
```

`overload_by_arguments.rs` pins the four and fails without the change. Nested in the operator path it is asked **before**
the arity choice, so the member list that choice builds is not built at all when the arguments decided; in the member-call
path the overload **count** is asked before the argument types, so a name with one declaration costs one posting query.
Measured: a warm diagnostics pull of `LuaParser/src/File/LuaSource.cpp` is **215 ms**, against **376 ms** for that file's
`assign-type-check` alone in the round that measured the member list.

**What is still not ranked**, and it is the next step rather than a defect: a candidate whose parameter needs a
**user-defined conversion** is `Unranked` (a check must not invent one), and a template candidate is deduced rather than
ranked. Both are documented at the top of `sema/overload.rs`.

## Lua 5.5.0 again: five reports, three causes, and one answer

Reported with screenshots on `lua-5.5.0/src`: the hover on `PF_VAHID` says "Expands to `PF_VAHID`"; `previous` is
coloured as a type; the hover on the parameter `level` says `LUA_API int level`; `ci_func(ci)->p` has no type, so `p`
cannot be resolved; `lua_State`'s layout is "not computed".

**The question asked with the first one — "you compared against the compiler, so why is the expansion still bad?" —
has a measured answer: the expansion is not what was wrong.** For these files the cook is right: `PF_VAHID` is `1` in
the value and `1` in the rendering, and `ci_func(ci)` is expanded in the rendering of `ldebug.c` (0 occurrences of the
name, the cast it stands for in its place). The defects were in the layers that decide *which reading answers*, and in
one type rule that the rendering then needed.

**Fixed, with measurements:**

* the hover's expansion. `the_expansion_of_a_macro` builds a synthetic source and cooked it against an environment
  keyed by **positions in the hovered file**; a definition from an included header sits at the `#include` (offset > 0)
  and was therefore not in force at the synthetic source's offset 0, so the name came back as its own expansion.
  `configuration_from_environment_as_of` reads the timeline at the moment of the use. `cast_byte(~PF_VAHID)` now
  expands to `1` (`dcc74a06`);
* the rendering after an eviction. The in-memory table holds eight renderings, and an index pass fills it, so
  `the_rendering_offset` answered `None` for a file that had been rendered — the hover's rendering fallback could not
  map a position at all. A miss now reads the persisted record, validated against the text (`dcc74a06`);
* the comma operator. `a, b` had no type rule, and `check_exp(c, e)` is `(lua_assert(c), (e))` — so `ci_func(ci)`'s
  expansion was typeless and `->p` had nothing to look the member up in. `(a, b)` is `int` for two `int`s and `double`
  for an `int` and a `double` (`ff0e7143`).

**Still open, each measured:**

1. **A parameter's card takes the enclosing function's specifiers.** `level` at `ldebug.c:162` is a parameter of
   `LUA_API int lua_getstack (lua_State *L, int level, lua_Debug *ar)`, and the card's code block reads
   `extern int level` — `LUA_API` expanded to `extern`. The fact is right (`ldebug.c:162:45`); the *line* is built from
   the declaration's leading specifiers, which are the function's, not the parameter's.
2. **A position inside a macro invocation is answered by the file's own text.** `hover` asks `hover_of_the_program`
   first, and that path returns the "type is not known" explanation for `p` in `ci_func(ci)->p` — a definitive answer,
   so `hover_of_the_rendering` never runs. The rule wanted is "a position inside an invocation is a question only the
   rendering can answer".
3. **`RenderedSpan::written` is `None` for every token of an expansion**, argument tokens included — `written_at`
   decides by the *invocation's* `written_in`, and an argument token was written in the file being cooked. So even
   asking the rendering first, a file position inside `pcRel(a, ci_func(ci)->p)` maps to the expansion's **start**
   rather than to the token under the cursor (measured: file offset 841 → rendering 215, whose `->p` is at 353).
4. **Semantic tokens colour a member as a type.** `ci->previous` is `CallInfo::previous`, and the token type served is
   `type` / `entity.name.type`.
5. **`lua_State`'s layout is "not computed".** Its members are `StkIdRel`, a struct of the same header; the layout
   writer refuses at the first member it cannot size and names all of them.

**Two more of the five, fixed in the same round:**

* **A field declared with an elaborated-type-specifier is a field, not a type** (`a4ba8c4c`). `struct CallInfo
  *previous;` is `DeclSpecifierSeq [struct, NamedType(CallInfo)]` plus a declarator; the walker looked for the tag
  *inside* the keyword node, found nothing, and bound the declarator's name as the class. Measured: `inspect facts` on
  `lstate.h` answered `190:20 previous type in CallInfo`, and `CallInfo`'s member list held a member named after the
  tag. Now `variable in CallInfo type `CallInfo*``, the tag is bound in the nearest enclosing **namespace**
  ([basic.lookup.elab], not into the class), and only when that scope does not declare it already. `class Widget w;`
  gains its other half too: `w` is bound as a variable. The semantic-token colour follows the fix —
  `a_structs_field_is_a_property` (properties, not types, on a fixture of the shape). `FORMAT_VERSION` 40 → 41.
* **A local's card is not its enclosing declaration's annotations** (`7903cf1b`). `the_attributes_before_a_declaration`
  starts at the declaration the fact's offset is in, which for a parameter is the function — so `LUA_API`
  (`#define LUA_API extern`) became the parameter's annotation and `level` was drawn `extern int level`. A local gets
  none; the sentence beside the block already says "declared in a function body". Measured: `int level`.

**What is left of the five:** the `ci_func(ci)->p` chain (hover asks the file's own text first; the mapping into a
rendering cannot name a token written inside an invocation's argument list) and `lua_State`'s layout.

## Greying out inactive code — implemented

The feature request recorded in the since-deleted `inactive-regions.md` is now implemented, server and VS Code
client: `RenderedCooked::inactive` → the cooked record (`COOKED_VERSION` 2 → 4, with `assumed_undefined`) →
`Session::inactive_regions` → clangd's `textDocument/inactiveRegions` notification (`version` in the params, sent when
a cook commits, an empty list on `didClose`) → the decoration in `E:\vscode-cppls` (opacity, stale-version drop, clear
on restart/stop/close, `cppls.inactiveRegions.enabled`).

The one decision that changed the design: **the answer is only painted when the configuration knew**. The probe's
numbers — 77 regions over the 61 Lua sources, 235 over the CRT headers — were measured with an empty macro table, so
they are an upper bound of a project without a configuration; every one of those branches was decided by C's
undefined-is-zero rule, which is a guess. `InactiveRegions::is_exact()` is that gate, and a non-exact answer sends an
empty list instead of grey.

What is left: the program path (a file read as a unit answers from its per-file cook — same configuration, same spans,
only slower), and the hover line ("this region is excluded by the configuration").

## Lua 5.5.0 again: a false `missing-return`, and where `ci_func(ci)->p` stops

Two reports from an editor on the C project, measured against `C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0`.

### 1. `missing-return` on `l_noret` — fixed

`static l_noret typeerror (lua_State *L, …)` was reported as *"declared to return `l_noret` and its body has no
`return`"*. `l_noret` is `void __declspec(noreturn)` (llimits.h:194-204), so the rule's premise was wrong twice over:
it asked **the word `void`** rather than what the declaration promises, and it treated *a name it cannot resolve* as
evidence of a value. The same keyword test had the opposite defect, measured on a fixture: `void *p() { }` — a pointer
return is a value — was **silent**.

The rule now reads the declaration in three answers (`the_promise_of`): a pointer or reference returns a value, `void`
returns nothing, another built-in returns a value, and a **name** is followed through the index — `typedef void
nothing` is `void`, a class is a value, and a name nothing declares is `NotKnown`, which is silence. Measured with
`inspect diagnostics --cook` on one fixture:

```text
void *buffer() { }                          reported (was silent)
int count() { }                             reported
typedef void nothing; nothing stop() { }    silent   (was reported)
typedef int myint; myint count() { }        reported
static UNRESOLVED_TYPE stop() { }           silent   (was reported — the shape of the Lua report)
```

The Lua project is unchanged by the fix where it should be: `missing-return` appears **nowhere** in its 61 sources
(`ldebug.c` still answers its 3 unrelated findings). `tests/checks.rs`'s
`missing_return_reads_the_declared_type_rather_than_the_word_void` pins all five shapes.

### 2. `ci_func(ci)->p` — where it stops, measured

`inspect type` on `ldebug.c:101`:

```text
col 26  `luaG_getfuncline(…)` is `int`          the call around it types
col 36  `ci` is `CallInfo*`                     the argument types
col 38  not known — UnknownType("ci_func")      the call the macro spells
```

`ci_func(ci)` is a `CallExpr` whose callee is the **macro** `ci_func`; the callee's name lookup answers `Known::No`
(nothing declares it — it is a `#define`), and `type_of_a_call` has no arm for that: the only thing it can do with a
callee that declares nothing is `UnknownType(callee)`. The expression is fine once expanded
(`(&((union GCUnion *)…->cl.l))->p`, which is `Proto *`), so the type exists — in the **cooked** text, which the
written query never asks.

The hover then says exactly what the editor showed: *"The type of `ci_func(ci)` is not known here, so `p` cannot be
resolved."* (`hover/mod.rs`'s `member_markdown`, when `type_of_the_expression` is not `Known`.)

**Two ways forward, in the order that fits the architecture:**

1. **Ask the rendering when the written answer is not known.** The mapping already exists
   (`RenderedCooked::the_rendering_offset_of`), and the object here is written *outside* the invocation's argument
   list, so its own span maps (measured with `expansion_spans_probe`: `->p` has `written: Some`). The work is: map the
   object's range, find the node in the cooked tree that *is* that expression, and type it there. Open question: the
   "innermost node at this offset" is not the object — the rendering's object is a parenthesised chain — so the node
   search has to take the **outermost expression whose written range equals the mapped one**, which is a rule worth
   writing down before writing it.
2. **Type the macro body with the arguments substituted.** The general answer, and the expensive one: a macro body is
   text, its parameters have the arguments' types, and the engine would need a small expansion-and-type pass. It would
   fix every `MACRO(x)->member` in every C project, which is a real class and not one expression.

Nothing is implemented for (2). Item 1 is the next step; until then the popup's explanation is accurate about *why*
(it does not claim the code is wrong).

**The first attempt at (1), reverted, and what it measured.** Asking the rendering in `member_markdown` reached the
crossing and answered the wrong expression: `the_rendering_offset_of` maps the object's first written byte to the
expansion's first cooked byte ✓, but `expression_at` answers the **innermost** expression containing an offset — and
at the first byte of `ci_func(ci)`'s expansion that is `(void)0`, the check macro's own first half, whose type is
`void`. The card then said "`void` has no member `p`". The outermost expression starting there is the one to type, and
the first version of that search found no node (it is a `ParenthesizedExpression` in the unit rendering, whose first
token belongs to the comma expression rather than to its own node), so the attempt was reverted rather than left
half-working. The shape of the rule to write: **the largest expression that starts at or before the mapped offset and
ends at or after the object's last mapped byte**, which is also the shape that survives a rendering whose parens the
renderer elided.

That is where this stands: the false `missing-return` is fixed (`a324f05f`), the type of `ci_func(ci)->p` is not.

## A C header's function declaration did not go to its implementation — fixed

Reported after the two above, from the same project: "a function defined in a C header cannot go to its
implementation". Measured with `crates/cpp_ls/tests/implementation_c_probe.rs` on Lua 5.5.0 — `lua.h`'s
`lua_newstate` answered **0** implementations while the body is in `lstate.c:335`.

The rule in `handlers/implementation` decided "is this fact a definition" from **the text**: a declaration ends with
`;`, a definition with `}`. That was written when the fact had no such flag, and it is wrong for every C project:

```text
lstate.c  lua_newstate  definition true   range 8508..8560  text `*lua_newstate (lua_Alloc f, void *ud, unsigned seed)`
```

— the fact's range covers the **declarator**, not the body, so the text never ends with `}`. `DeclFact::definition`
carries the answer (`crates/cpp_code_analysis/src/cache/summary.rs`, "a function whose declaration contains a compound
statement"), and the handler now reads it, keeping the text test as a fallback for a fact that says `false` while its
range does end with a `}`.

Measured after: every declaration in the probe's three headers answers its source — `lua_newstate` →
`lstate.c:335`, `lua_gettop` → `lapi.c:173`, `lua_settop` → `lapi.c:178`, `luaG_runerror` → `ldebug.c:856`,
`luaG_typeerror` → `ldebug.c:756` — and the C++ probe (`implementation_real`, `EmmyLuaCodeStyle`) still passes.
`tests/implementation_declaration.rs` pins the C shape, the C++ out-of-line member, and a declaration no file defines.

**One shape still answers nothing, and it is a different defect:** `lua.h` writes `LUA_API int (lua_pcall) (…)`, a
**parenthesised declarator**, and the name lookup answers `Unknown(UnparsableName)` — so "go to definition" on that
token fails too, not just implementation. That is the name resolver's, not this handler's.

## A parenthesised declarator had no parameter list — fixed

The next report from the same editor: hovering `lua_setfield` printed `extern void lua_setfield(…)` and no inlay hint
was placed. Lua 5.5.0 writes its whole public API as `LUA_API void (lua_setfield) (lua_State *L, int idx, const char
*k);`, and the tree puts the `ParameterList` **beside** the declarator (`InitDeclarator (Declarator
"(lua_setfield)", ParameterList …)`) rather than inside it — so `CppDeclarator::get_param_list`'s direct-child search
found nothing, the fact had no `parameter_list`, and the parameters were filed as **file-scope variables**.

`CppDeclarator::get_param_list` now reads that sibling, only when the declarator is the declaration's own (parent is
the `InitDeclarator`) and holds no `*`/`&` — `void (*bar)(int a);` is a *variable* of type `void(int)*` and its list
belongs to the pointed-to type. The analysis's two readers ask through it and rise through the declarators around a
name, stopping at a pointer; the list is asked for **before** that guard, so `int *f(int a)` keeps its parameters.

Measured on the project: `lua.h:281:16 lua_setfield` went from `function returns void` to `function returns void
parameters (lua_State *L, int idx, const char *k)`; the hover card reads the real signature; `lauxlib.c` now places
**301** parameter hints, with `idx:` and `k:` at the `lua_setfield(L, -2, "__name")` call. The inlay guard
(`a_call_through_a_function_pointer_is_not_hinted`) still passes: a function-pointer *type*'s list is not a
function's. `FORMAT_VERSION` 41 → 42, because a stored summary carries the empty list and `cpp_parser::VERSION` does
not move for a reader's change.

## `MACRO(x)->member`, after the typedef fix: the last two steps, named

The chain in `ci_func(ci)` is `(&((union GCUnion *)…->cl.l))->p`. With the alias walk following a pointer `typedef`
(commit `f952a0e`, `StkId` → `StackValue`), the expansion types almost to the end, and
`crates/cpp_ls/tests/macro_member_probe.rs` names the step that stops it:

```text
..p  on "(ci)->func"        object typed StkIdRel
..gc on "(((&((ci)->func.p)->val) )->value_)"   object typed Unknown(UnknownType("(ci)"))
..l  on "(((union GCUnion *)(…))->cl"           object typed Unknown(NotDeclaredHere("union GCUnion::cl"))
```

Two findings, both measured:

1. **An elaborated-type-specifier spelling is not read by the *type engine's* member lookup.** `members_of("union
   GCUnion")` answers its 8 members (`cl`, `gc`, `h`, `p`, `th`, `ts`, `u`, `upv`) and `members_of("Closure")` its 2 —
   so the member list query normalises the `union ` prefix, while the access behind `->`/`.` does not: it asks for
   `union GCUnion::cl` and comes back `NotDeclaredHere`. The engine's class comes from the cast's type, which C allows
   to be written elaborated (`(union GCUnion *)`), and a C project that typedefs its unions writes it that way.
2. **One `UnknownType("(ci)")` remains inside `(&((ci)->func.p)->val)`** while the same `(ci)` types as `CallInfo *`
   in `(ci)->func.p` two steps earlier. Parentheses are not the cause — a fixture (`((x))`, `((s))->n`) types both
   correctly — so it is something about the *rendering's* spelling of that run (the writer leaves a space before the
   closing parenthesis: `)->val) )`), and the next measurement is that node's tree, not a guess.

Nothing else in the chain is unknown: `StkIdRel`, `TValue`, `GCUnion`, `Closure` and `LClosure` all answer members once
the program reading is committed (`TValue`'s body is written by a macro, `TValuefields`, so its two members exist only
in the rendering — `session.commit_a_unit_reading` is what files them, and the probe shows `[]` before it and
`["tt_", "value_"]` after).

## The declarator's shape was not reasonable — measured, and fixed

Asked after the parenthesised-declarator fix: whether the tree that needed it is the tree it should be. The answer is the table printed from the grammar by
`crates/cpp_parser/tests/suite/declarator_shapes.rs`. Two defects, both consequences of one rule:

* **A suffix after `( ptr-declarator )` is left as a sibling of the declarator**, not inside it
  (`int (*fp)(int);` is `Declarator[(*fp)] ParameterList`). Six of the surveyed shapes do it, and the "list is my
  parent's sibling" rule in `CppDeclarator::get_param_list` is right for one of them (`(named)(int)`, the Lua case) and
  wrong for the rest.
* **A function returning a function pointer is recorded as a variable**: `int (*maker(int a))(int b);` gives
  `maker variable type int(int)*`, and `void (*signal(int, void (*)(int)))(int);` the same. That is a wrong answer at
  the fact level, not a missing one, so its calls, its signature help and its hover all answer about an object.

**Fixed.** The grammar attaches the suffixes to the declarator that opens at `(`
(`parse_init_declarator`'s parenthesised branch and `parse_parenthesised_declarator`), `CppDeclarator::get_param_list`
is a direct child again, `CppDeclaration::get_param_list` walks for the declarator that owns both a name and a list, and
`sema::scopes`'s function-or-variable question is structural (rise from the naming declarator, list first, stopping at
a pointer) — which also stopped the returned pointer's parameter (`b` of `(int b)`) being filed at file scope. Measured
after: `int (*maker(int a))(int b);` is a **function** with parameters `(int a)`, `void (*signal(…))(int);` likewise,
`int (*fp)(int);` stays a variable, and the whole suite is green.

**Including the type half:** a function returning a function pointer now records the returned type from the shape —
`maker` answers `returns int (*)(int b)`, `signal` `returns void (*)(int)` — through
`a_parenthesised_return_type`, which reads the operators between the declarator's `(` and the name and the suffix after
the name's own parameter list.

## `MACRO(x)->member` — the chain, and the two rules a rendering crossing needs

The last of the Lua reports, and the one that had been open the longest: `ci_func(ci)->p` in `ldebug.c:101`. Two
separate defects, both measured, and neither was in the macro reader.

### 1. The expansion types, and what stopped it was a **depth bound**

The expansion is `(((void)0),((((void)0),(&((union GCUnion *)(…)->cl.l))))))->p` — a function-callish, cast-laden,
deeply **parenthesised** chain, not a deep type. `MAX_TYPE_DEPTH` was **8**, and every parenthesis and every comma
operator is a level; traced with `CPPLS_TRACE_TYPES`, the object `…->value_` asks `ci` at depth **10** and the whole
expression needs more. Past the bound the walk answered `UnknownType("ci")` — the spelling at the level it stopped on,
which is why the symptom named `ci` rather than anything the reader could act on.

Raised to **24**, with the measurement in the constant's own documentation. Nothing else about the bound changed: a
cycle is still caught by `TypeMemo::enter` rather than by it.

### 2. The crossing: **span, not first byte** — and a member is asked as a member

With the chain typing, the remaining question was where to ask it from. The file's own text has no type for
`ci_func(ci)` — the object is a call to a name nothing declares — so the answer has to come from the rendering, and
three rules were measured on the way:

* **A written expression is found in the rendering by its span.** The first byte of an expansion is a `(`, and what
  contains it is the whole `…->p`, which answers the *member*'s type rather than the object's. Because the writer maps
  an invocation's trailing bytes to the end of what it expanded, the end of a *written* offset inside the invocation is
  not a boundary either; what is exact is the byte **after** the object (`->` of `ci_func(ci)->p` is written text), so
  the expansion is the largest expression between the expansion's start and that operator.
  `sema::resolve::the_largest_expression_between`.
* **A member of an expansion is asked as a member, not as an expression.** `tsvalue(o)->shrlen` crossed as one span
  answered `TString*` — the *object*'s type, because the span ends inside the expansion — while the member is
  `ls_byte`. The object's type crosses correctly (its end is the expansion's end), and the member is then found in it.
* **A name alone is not a span question.** A macro's name has no rendering counterpart at all, so crossing a bare
  `ci_func` landed on whatever surrounded the invocation and answered the enclosing call's type. It answers "not known",
  which is the truth; the hover answers that word with the macro's definition.

### Measured after

```text
hover at ldebug.c:101:40        Proto* p / `LClosure::p` — a variable of type `Proto*`, in `LClosure` / lobject.h:708
hover at lapi.c:440:61          ls_byte shrlen / `TString::shrlen` / lobject.h:408
type  at ldebug.c:101:40        `ci_func(ci)->p` is `Proto *`
type  at lapi.c:440:61          `tsvalue(o)->shrlen` is `ls_byte`
type  at ldebug.c:101:34        `ci_func(ci)` is `LClosure*`
type  at ldebug.c:101:30        not known — UnknownType("ci_func")   (a name, not an expression)
```

Pinned by `crates/cpp_code_analysis/tests/macro_member_types.rs` (a hermetic Lua-shaped fixture), and
`crates/cpp_ls/tests/macro_member_probe.rs` remains the instrument: it prints the type of every member access along the
real expansion.

**Still open, and unrelated to macros:** `type` on the argument of a call whose callee is a function defined in the
same file answers the *call's* type (`currentpc(ci)` at `ldebug.c:101:46`), because the argument's node is not the
innermost expression there. `hover` on the same position answers the declaration, which is right.

## The subscript's overloads, through an alias — `json["k"]` and `const std::string cs; cs[0]`

The item the `is_string` round left open: *"a class whose overloads return different types needs a choice by argument
conversion"*, and `json["data"]` answered `basic_json&` only because every `operator[]` of `basic_json` returns
`reference`, so taking the first member happened to be right.

The ranking already existed (`the_one_the_arguments_select`, `crate::sema::overload::choose`) and was already wired
into the subscript arm; what it needs is the **candidate list**, which is `member_candidates` — a *scope* posting
keyed by the class's **qualified** name. Following an alias handed it the target's own spelling, and the target is
written unqualified inside the alias's namespace:

```cpp
namespace ns { struct Table { int& operator[](int); double& operator[](const char*); }; using Json = Table; }
```

so the query asked the index for `Table` and found nothing, while `members_of("Table")` listed every member. With no
candidates the ranking never ran and the first declaration was taken. Measured on `nlohmann::basic_json`:

```text
before   [candidates] basic_json::operator[]:          0 declaration(s) named, 0 candidates
after    [candidates] nlohmann::basic_json::operator[]: 242 in scope, 8 named → 8 candidates:
         (size_type idx) / (const object_t::key_type& key) / (T* key) / (const json_pointer& ptr), each const and not
```

`operator_returns_of_with_arguments` now tries the **qualified** target first — `nlohmann::basic_json` for
`nlohmann::json` — and keeps the unqualified one as the fallback, for an alias whose target is a class at file scope.
Both spellings are asked because the two queries are not the same question: `files_declaring` searches by **name** and
answers the unqualified one, `declarations_in` is keyed by the **qualified** one.

### Measured

```text
                                  before        after
ns::Json table; table["k"]        int&          double&      (the `const char*` overload)
ns::Json table; table[0]          int&          int&         (unchanged — the first declaration was right)
const ns::Crate crate; crate[0]   const int&    const int&   (constness already chose; kept as a guard)
nlohmann::json j; j["emmylua"]    basic_json&   basic_json&  (every overload returns `reference`; now by choice)
```

`crates/cpp_code_analysis/tests/subscript_overloads.rs` pins it in two files (a header declares the class and the
alias), and it **fails without the change** — `table["k"]` answers `int&`. A class in the file being edited never had
the defect: its members come from its own scope tree, where the bindings are filed under the name they were declared
with. `CPPLS_TRACE_INFERENCE` prints the candidate list and the chosen member (`[candidates]`, `[rank]`), which is how
this was found.

## The global allocator carried a debugging counter — removed

Found by reading `crates/cpp_ls/src/bin/cpp_ls.rs` rather than by a failing test, which is the point: the
`#[global_allocator]` installed in every release was not `MiMalloc` but a wrapper around it.

```rust
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        cpp_ls::note_allocation(layout.size());      // -> cpp_code_analysis::alloc::note
        unsafe { MiMalloc.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        cpp_ls::note_release(layout.size());
        unsafe { MiMalloc.dealloc(pointer, layout) }
    }
    // realloc and alloc_zeroed the same
}

#[global_allocator]
static GLOBAL: Counting = Counting;
```

The counter was **off by default** — `CPPLS_TRACE_ALLOCS` decided it once at start-up — so the bytes it collected cost
nothing when unset. What it always cost is the path: every allocation, free and reallocation in the process entered a
Rust function, forwarded to `cpp_code_analysis`, and made a **relaxed atomic load** before the allocator was called.
An instrument whose numbers nobody reads, paid for by every user on the hottest path a program has.

### What it fed, and what each trace became

| trace | numbers | now |
| --- | --- | --- |
| `[memory] live … peak … churn …` per pump round | the counter's | **deleted** |
| `[memory] STOPPED … CPPLS_LIVE_CEILING_MB` | the counter's | **deleted** (the guard cannot exist without a live total) |
| `[pull] prepare done / diagnose_file / query returned`, MB | the counter's | **deleted**; `[memory] pull:`, the session's own report, stays under `CPPLS_TRACE_MEMORY` |
| `[inlay] … live → live, churn, peak` | the counter's | **deleted**; the two hint counts and their timings stay under `CPPLS_TRACE_INLAY` |
| `[tables]` | `Session::memory_report` | **kept**, under `CPPLS_TRACE_MEMORY`, without the process total beside it |
| `[inbox] [rounds] [read] [program]` | none | re-gated: **`CPPLS_TRACE_PUMP`** (they were the pump's own account, riding the allocator switch) |
| `[query] permit / gate / session / ran`, ms | none | re-gated: **`CPPLS_TRACE_QUERY`** |

Deleted with it: `cpp_ls`'s six forwarding functions, `cpp_code_analysis::cache::alloc`, `probes::allocations`,
`probes::live_ceiling_mb`, the `CPPLS_TRACE_ALLOCS` and `CPPLS_LIVE_CEILING_MB` variables, and the two calls in
`run_ls`/the pump that read them. The binary is now what it should have been:

```rust
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
```

**What remains is a different question, and it is still answered.** `Session::memory_report()` estimates what the
analysis *holds by structure* — summaries, cooked readings, units, renderings, views, the index tables — and
`CPPLS_TRACE_MEMORY` prints it, with a file dropped at `%TEMP%\cpp_ls-memory.txt` when the queue drains. That is not
the resident set: the gap between them is the allocator's slack, which is exactly why a process-level question needs a
profiler rather than a counter in the allocator. The memory numbers recorded earlier in this file were measured with
the counter and stand as measurements; the instrument that produced them does not.

### The `perf-probe` feature was the same defect, and already dead

`cpp_code_analysis` had an opt-in `perf-probe` feature whose marks (`crate::perf::mark("cook")` and seventeen others)
recorded allocation counts per phase. Its **only driver was `tests/allocation_probe.rs`**, a probe the test
reorganisation deleted (`2dc17c8d`), so from then on every mark recorded a counter nothing incremented: `perf::note` had
**no caller anywhere in the tree** (on the parent commit, `git grep perf::note` finds nothing) and `per_phase()` had no
caller either. The feature still compiled — the baseline `cargo test --all-features` run is what proves that — and it
measured nothing, which is worse than not existing. So the feature went: `cache/perf.rs`, the
`#[cfg(feature = "perf-probe")]` gates and marks in five files, the feature table, and the unused `mimalloc-safe`
dev-dependency it needed. The comments that
quote what it measured (`24.9 M allocations` of a `import std;` read, `11.3 M` for the reflection pass) stay, labelled
as a removed instrument's numbers.

### The rule is a test now

`crates/cpp_ls/tests/suite/global_allocator.rs`:

* the binary's source must contain `static GLOBAL: MiMalloc = MiMalloc;` and a `#[global_allocator]`;
* **no `crates/*/src` file may `impl GlobalAlloc`** — a test may install its own counter (that is the only way to count
  allocations), but a shipped crate may not.

A global allocator has no runtime API to ask, so the test reads the source — the same trade
`cpp_code_analysis/tests/suite/no_global_state.rs` makes for `thread_local!` and process-wide caches.

### Verification

```text
cargo test --release --workspace --all-features                      3 354 passed, 0 failed, 30 ignored
cargo clippy --workspace --all-targets --all-features -- -D warnings 0 warnings
cargo fmt --all -- --check                                           clean
```

The targets, from that run:

| target | passed | ignored |
| --- | --- | --- |
| `cpp_code_analysis` lib / suite | 895 / 1 140 | 3 / 9 |
| `cpp_parser` lib / suite | 52 / 514 | 0 / 0 |
| `cpp_toolchain` lib / suite | 161 / 21 | 0 / 0 |
| `cpp_ls` lib / suite | 197 / 116 | 1 / 10 |
| `cpp_formatter` lib / suite | 116 / 128 | 2 / 1 |
| `cpp_check` / `cpp_inspect` / `edit_version` | 6 / 1 / 4 | 0 |
| doctests | 3 | 4 |

**The end-to-end half is the part that matters for this change**: ten `cpp_ls` suite modules spawn
`target/release/cpp_ls.exe` — the binary with the new allocator — and speak LSP to it (`handshake`, `latency`,
`progress`, `idle`, `diagnostics_stable`, `lsp_status`, `real_project`, `watch_rescan`, `a_header_language`,
`formatter_choice`). They pass, so the allocator is proven by a running server and not only by a compile.

**The guard was checked against the defect, not only against today's tree.** A temporary
`crates/cpp_ls/src/temporary_allocator_probe.rs` holding an `impl GlobalAlloc` made
`no_shipped_source_wraps_a_global_allocator` fail with that path; deleting the file made the two allocator tests pass
again.


## Found while reading for the allocator: the cleanup's duplicates and citations — fixed

Same pass, not the same change. Everything below was found while reading for the allocator, measured, and then fixed
in the same round; the counts are the before numbers.

### Five headings this file held twice

Two commits left a second copy of a block instead of replacing the first:

```text
heading                                               first copy   second copy   commit
`std::endl` drawn as a variable …                     line 6       line 113      83e7b632 (+94 lines)
An inlay hint rendering a deduced type … **fixed**    line 37      line 144      83e7b632
What this run did learn about `inspect` itself        line 65      line 172      83e7b632
Diagnostic false positives: …                         line 73      line 180      83e7b632
Why `std::vector` itself is missing …                 line 413     line 616      4e70be1f (+32 lines)
```

For the first four, the first copy was `83e7b632`'s updated text — it ends "After the fixes … 32 findings to 0" — and
the second was the older copy, which still listed the `is_convertible` finding as open. The two copies were
byte-identical outside the diagnostic section, so the older one went: **107 lines removed**, and one blank line put
where the heading had run into the previous paragraph.

The `std::vector` pair was not a copy: `4e70be1f` gave the long body (until then part of the `size()` section) its own
heading, while a shorter section with that heading already existed (added by `c89f6360`). The long copy is the newer
record — it carries "Fixed, measured 2026-11-16" — and it ended on a dangling paragraph starting "— and the index holds
no fact for it", left by an earlier cut. That fragment went (**6 lines**; its claim is contradicted by the "Fixed"
paragraph above it). The older section keeps its measurements under its own heading — `std::vector` itself was
missing because nobody asked for the header's refinement, which is what `c89f6360` was about. The file went from
**3 297 to 3 185 lines and 67 to 63 headings**, and each of the four duplicated headings now appears once.

### 149 citations of deleted documents, in `crates/`

Counted at the commit before this round:

```text
docs/parser-toolchain-review.md       45      docs/indexing-design.md   9      docs/memory.md          1
docs/language-server-review.md        35      docs/indexing-review.md   8      docs/formatter-diff.md 10
docs/code-analysis-review.md          23      docs/module-cache.md      9      status.md               7
                                                                               "see the audit"         2
```

The rule is the one `a7de65fa` set for four other documents: **name the live document that holds the record, and let the
section numbers go with the document they belonged to.** The review documents are recorded item by item in
`docs/worklog.md` (its P1/P2/P3 lists, with the commit each item closed in), so those **103** citations point there and
keep their item code, which the worklog itself uses. The design documents are the cache rewrite's archaeology, which
`docs/open-questions.md` holds, so the **27** citations point there with no `§`; where the sentence said "phase 0/1/2
of …", the phase label was the deleted document's, and it now names the thing instead (the refinement queue, the L1/L2
split, the composition). The formatter differential's **10** point at the audit that produces it,
`crates/cpp_formatter/tests/suite/against_clang_format.rs`. `status.md`'s **7** and the two "see the audit" citations
had no live record and no fact that needed one — the sentence around each already said it — so the citation went and
the sentence stayed.

Two READMEs pointed at other deleted documents, and `docs/configuration.md` at the formatter differential; those went
with them. A relative-link check over every markdown file in the repository now finds no missing target. (`docs/worklog.md`'s
own mentions of the review documents are the record of those reviews, and are left, as `a7de65fa` left the ones it
found.)

### 26 test commands named targets the reorganisation removed

`cargo test … --test <name>` in 21 files, naming 21 distinct targets, none of which exists as a target since
`2dc17c8d` merged 152 files into one `tests/suite.rs` per crate. Each is now `--test suite <module>`; `format` uses
`--test suite format::`, because the bare `format` filter would also select `format_scanner`. `translation_unit`'s two
arguments were a target and a filter, so it keeps only the filter (`--test suite a_scope_macro`). Measured with
`--list`: `--test suite format::` lists **17** `format::` tests and no `format_scanner::` one, and
`--test suite read_ahead_probe` lists **2**.

One was not rewritten: `crates/cpp_parser/src/probes.rs` told a reader to run `--test parse_cost`, and `parse_cost.rs`
was one of the probes deleted in `2dc17c8d`. The sentence that described it went; the measured numbers above it stayed.

### What is left of the same kind, measured

These are **not** the cleanup's residue but older debt the same scan found, and none of it is a markdown link, so the
link check does not see it.

```text
diagnostics-performance.md   6      plan-units.md         3      notes.md         2
perf-review.md               6      perf-progress.md      3      hover-render.md  1
inactive-regions.md          5      ranges.md             3
refinement.md                4      declarator-shapes.md  4
```

**37 named citations to ten further documents.** Three have live material to point at — `inactive-regions.md` (the
handler module), `refinement.md` (the stored-refinement suite) and `declarator-shapes.md`
(`tests/suite/declarator_shapes.rs`); the other seven are performance and planning notes with no live record.

**And 38 lines that lean on an unnamed plan**: "the plan's rule 3", "the plan's §5.2", "gate ①", "design note §2.1";
70 lines in `crates/` carry a `§` with no live document behind it. A re-point is not the fix here: the
sentence uses the plan as the *authority* for a rule, so the rule has to be stated in the sentence — most already state
it beside the citation — and the reference dropped. That is a prose edit per site, and it is the next pass.

## `__FILE__` and `__LINE__` in a macro's expansion — fixed

Reported from an editor, with a screenshot. The card for

```cpp
#define debug(x) std::println("DEBUG: {}: {} {}", __FILE__, __LINE__, x);
...
debug(println("Starting main function"));
```

said `__FILE__` was `""` and `__LINE__` was `2`. Two independent causes:

1. **`__FILE__` had no file to name.** `expand.rs`'s `a_position_dependent_macro` answered it with a hard-coded `""`,
   and nothing in the cook path passed a name — a placeholder whose comment even said "`__FILE__` has no name to answer
   here". Every rendering therefore carried `""` for a file, not just the card.
2. **The card's `__LINE__` counted a synthetic source.** `the_expansion_of_a_macro` builds `#define`s for the file's
   macros and appends the invocation, so the invocation sat on the line after them: measured, a `#define` on line 1 and a
   call on line 3 answered **2**.

### The fix

`expand_with_positions` now takes the file the run is written in (`Option<&Path>`), and `__FILE__` is that path as a
C++ string literal — `\` and `"` escaped. `cook_with_file` is the cook that supplies one; the unit cook passes each
frame's file, and `Session::render_cook`, `rendering_of` and the index's two renderings pass theirs, so the answer is in
the rendering and not only in the card. A text with no file behind it (a fixture) still answers `""`, which is the
honest answer there.

The card pads the synthetic source **before** the invocation so the invocation starts on the line the reader wrote. The
first version appended the padding after the invocation — the test still said `2`, which is how that mistake was caught.

Measured through `cpp_inspect` on the reported shape (the fixture's own path, since a fixture is a temporary file):

```text
std::println("DEBUG: {}: {} {}",
    "C:\\Users\\zc\\AppData\\Local\\Temp\\cppls-inspect-345776-47000\\main.cpp",
    3, println("Starting main function"));
```

The argument keeps its call — the card in the report appeared to show `println` alone, and on this shape the expansion
is the whole `println("Starting main function")`; if the original text had the argument split across lines, that is a
separate report.

### What the file is called: the command's spelling, not the path — done

`__FILE__` answers the *presumed* name: the spelling on the compiler's command line. The analysis now keeps that
spelling. `CompileCommand` gained `file_as_written`, filled from `compile_commands.json`'s `file` and
`build_database.json`'s `source` **before** the path is resolved against the directory; `CompilerConfig::presumed_name_for`
answers it for a file. It reaches the cook through three doors:

* the unit cook passes the root frame's spelling (`TranslationUnit::root_presumed_name`, set by the session that has
  the database) and an included file's own path, which is what a compiler reports inside a header;
* the per-file cooks (`Session::render_cook`, `rendering_of`, the index's two renderings) look it up per file;
* the hover card does too, so the card says `"src/a.cpp"` rather than the path the analysis opened.

The path remains the fallback: a header, a file outside the database, a unit built from summaries alone. Measured: a
database entry `{"directory": "/p/build", "file": "../src/a.cpp"}` answers `file_as_written == "../src/a.cpp"` and
`presumed_name_for("/p/src/a.cpp") == "../src/a.cpp"`; a card over that database expands `__FILE__` to `"src/a.cpp"`.

**The key has to cover it.** A rendering with `"src/a.cpp"` is not the rendering with `""` or with the resolved path, so
`context_hash` folds the presumed name and `FORMAT_VERSION` moved 57 → 58. `COOKED_VERSION` is the record's format and
does not move for what the text says.

### `#line`, which is the standard's other half

`Directive::Line` was parsed and then dropped by the cook's `_ => {}`, so `#line 100 "x.c"` changed nothing. It is
applied now: the cook keeps `(presumed_file, line_base)` across directives, passes them to each run, and the expander
adds the base to `__LINE__` and uses the name for `__FILE__`. The directive's own line keeps its number; the **next**
line is `N`. A `#line` in a region nobody compiles changes nothing.

Measured: `int a = __LINE__;` (1), `#line 100 "x.c"`, `int b = __LINE__;` (100), `__FILE__` → `"x.c"`, and the next
line's `__LINE__` is 102 — the numbering continues from 100.

**A defect the `#line` test found, one layer under it:** a `__LINE__` written directly in a run answered the run's
**first line**, because `a_position_dependent_macro` used `region.start_offset` before the token's own offset (the
region fallback exists for argument prescans, whose synthetic ranges are not in the run). The token's own offset is
used when it falls inside the run; the prescan still gets the region. Without that, two `__LINE__`s in one run answered
the same number.

**Also covered, in the same pass.** The hover card's synthetic source now states its position with a `#line` of its own:
`presumed_position` walks the file's directives with the same `a_line_directive_changes` the cook uses, so a `#line`
in force at the call is what the card reports (measured: `#line 100 "x.c"` on line 2 makes the call on line 4 answer
`101`). GNU linemarkers (`# 100 "x.c" 1`) are read as `#line` by `parse_directive_tokens` — the token after the `#` is
the number, not a directive name. And a `#line`'s arguments are macro-expanded before they are read, so `#line L` and
`#line __LINE__` work; the arguments are expanded as their own run (`expand_with_positions` over
`Directive::Line`'s tokens), and the `__COUNTER__` count is carried back.

**`#if __LINE__` is answered too.** The condition evaluator is handed a macro table, and a name with no `#define` is
C's `0` there; the two position names are bound by no `#define`, so `decide_a_condition` wraps the table in
`ThePositions`, whose `lookup` answers `__LINE__`/`__FILE__` with a one-token definition of the directive's position
(the `#elif`'s own line; a `#line` in force is applied). It is built only when the condition mentions one — directly or
through a macro body (`a_body_mentions_the_position`, depth-capped), so an ordinary `#if` pays a token scan and no
allocation. Measured: `#if __LINE__ == 1` on line 1, `#ifdef __LINE__`, `#if defined(__FILE__)`,
`#elif __LINE__ == 3`, and `#define L __LINE__` + `#if L == 2` all take the expected branch.

**What is still not covered**: `#if __COUNTER__` — the counter is mutable state and `MacroValues::lookup` answers by
reference, so a synthetic definition cannot advance it; and a `#line` that sits in a region nobody compiles is
correctly ignored by the cook, but the **card** does not evaluate conditions, so its `presumed_position` would apply
such a `#line` if one stood before the call.



Tests: `the_file_and_the_line_macros_answer_the_use`, `a_line_directive_changes_the_line_and_the_file`,
`a_line_directives_number_is_expanded` and `a_gnu_line_marker_changes_the_line_and_the_file` in
`cpp_toolchain/tests/suite/rendering.rs`; `a_condition_answers_the_line`, `..._the_definitions`,
`..._the_line_after_a_line_directive`, `an_elif_answers_the_line` and `..._the_line_through_a_macro` there too;
`a_macro_expansion_answers_the_line_of_the_use`, `..._the_file_of_the_use`, `..._the_commands_spelling_of_the_file`
and `..._honours_a_line_directive` in `cpp_code_analysis/tests/suite/macro_expansion.rs`;
`the_presumed_name_is_the_spelling_the_database_wrote` in `config.rs`. `cargo test --release -p cpp_toolchain -p cpp_code_analysis --all-features`: 895 + 1 144 + 162 + 30
passed, 0 failed, 14 ignored.

## CI's handshake suite ran unoptimised, and a 60 s budget is a release budget — fixed

`every_inactive_regions_publication_matches_the_version_it_names` timed out on CI with "the server did not answer
within 30s" — a message that printed the helper's `TIMEOUT` constant, not the **60 s** the test actually passed. The
test passes locally, and the difference is the **profile**: the workflow ran `cargo test --workspace --all-features`
(unoptimised), while `.cargo/config.toml` — since `8c632761` — says the suite is a measurement workspace and defines
`cargo t` as `test --release`.

Measured on Windows, same commit, same fixture, single test: **1.69 s** in release, **61.30 s** unoptimised. The
unoptimised run passed, at 61.30 s; a 2-core CI runner under the whole suite is slower, so one version's 60 s wait
ran out. The server log names the unoptimised cost: index slices, a "read a program as one piece: 1 file(s),
15000 token(s)" and cook slices about five seconds apart. It is slow, not racing.

The fix is the profile rather than a longer wait: `.github/workflows/test.yml` now runs `cargo test --release
--workspace --all-features`, which is what the repository's own `.cargo/config.toml` has asked for since it was
written. `wait_for_within` now prints the caller's `within` instead of `TIMEOUT`, so a future failure says the budget
it actually missed.

Not changed: the fixture (3 000 lines) is what makes the race window measurable, and in release it costs ~1.7 s.

## An elaborated tag declared a type, and hid the real one — fixed

`struct bits make_bits(...)`, `struct bits value;` and a `struct bits` parameter use the tag as a **type**, but the
scope builder bound every bodyless `StructDef` as a class declaration in that file. The summary then held a `bits` Type
fact with no members and no body, and it shadowed the definition in the header: from `bits.c`, `members --class bits`
answered **0** and `pointer->three` reported "`const bits*` has no member `three`", while `corpus.h`'s own summary held
all four (including the `_BitInt` ones). The same fabricated fact is what a jump on the parameter's `bits` resolved to
— the function's own line, not the struct.

The rule is the declaration's own: a **bodyless** class-like node declares the tag only when the declaration it sits in
has **no init-declarator** (`struct bits;`). A body is always a declaration (`struct bits { … };`, `typedef struct { … }
Name;`), and an elaborated specifier of another declarator is a use.

**The same shape on an enumeration had the same effect, by a different node.** `enum Color pick(void);` is a
`BuiltinType` holding a `NamedType` rather than a `StructDef`, and the elaborated-tag binding there (`class Widget w;`
gained its tag by the same code) filed `Color` in the file that used it: `define` on the parameter's `Color` answered
`bits.c:2:6` instead of the enumeration. A tag is now bound only when the declaration introduces nothing else —
`struct bits;`, `class Widget;` — which is the case [basic.lookup.elab] is about.

Measured on `C:\Users\zc\Desktop\c-project` (`corpus.h`'s four members):

* before: `members --class bits` from `bits.c` → **0**; hover `pointer->three` → "`const bits*` has no member
  `three`"; `define` on the parameter's `bits` → `bits.c:8:8` (the fabricated fact on `make_bits`'s line);
* after: **4** members from `corpus.h`; hover → `unsigned _BitInt(3) three`, `bits::three` at `corpus.h:11:25`;
  `define` → `corpus.h:10:8`.

And the enumeration fixture (`enum Color { Red, Green };` in `corpus.h`, `enum Color pick(void);` in `bits.c`): before,
`define` → `bits.c:2:6`; after, `corpus.h:1:6`.

Neither the content nor `context_hash` moved, so `FORMAT_VERSION` 59 → 60 makes the stale summaries unreachable.
Tests: `a_c_struct_in_a_header_has_its_members`, `an_elaborated_type_specifier_does_not_declare_the_tag` and
`an_elaborated_enum_specifier_does_not_declare_the_tag` in
`crates/cpp_code_analysis/src/index/project/tests.rs`.
## A GNU statement expression in a hover card is not formatted correctly — open

Linux trees expand several allocation macros through a GNU statement expression: `mark_allocated(...)` is
`alloc_hooks(mark_allocated_noprof(__VA_ARGS__))`, and `alloc_hooks` is `({ … })`, so the card shows a statement body.
`cpp_formatter::reflow` treats the `{` after `(` as an **initializer list** (`breaks.rs` and `layout.rs` classify a
brace introduced by `(` as `Brace::Nothing`), so its contents get no block indent and the line breaker wraps at
parentheses: reproduced with a fixture, the card prints `({ typeof(...) _res; if (...) {` then `struct alloc_tag ...` at
column 0, then `} else {` and `_res; })`, where clang-format writes `({\n    typeof(...) _res;\n    if (...) {\n …`.

The fix belongs in the formatter, not the card: a `{` whose `(` is a statement-expression opener is a **block** (the
parenthesis is transparent for line breaking), so its statements end lines and indent, and the matching `})` closes it.
That is a change to `breaks::logical_lines_with` and `layout::classify_braces`, with the shape clang-format 19.1.7
writes as the expectation, and a test on the fixture. Not done — this turn moved to the C member lookup and jump
defects (above), which were reported next.

## A parser review of the GNU/MSVC/Clang extensions — what held, what was fixed, what is open

A review of `cpp_parser`'s vendor-extension coverage was checked against the code, the parser suite and the compilers
on this machine. The claims and their verdicts:

| The review said | Verdict | Evidence |
|---|---|---|
| `Feature::NestedFunction` is defined but no grammar reads it | **confirmed, fixed** | a block-scope definition is accepted when the configuration is GNU **and** the family is C (g++ refuses the construct); the definition is a `Declaration` inside the body |
| `__auto_type` is not lexed as `auto` | **confirmed, fixed** | the lexer maps it to `AutoKeyword`, the specifier is a `BuiltinType`, and the hover deduces the initializer's type (`n = 2` → `int`); the fact keeps the spelling, as an ordinary `auto` fact does |
| `__alignof__` is not an `alignof` keyword | **confirmed, fixed** | the lexer maps it now; the tree is a `UnaryExpr` over the keyword |
| `__typeof_unqual__` is missing | **confirmed, fixed** | lexer + the three analysis spelling lists (`deductions`, `types`, `undefined_name`); a GNU parse makes it a `BuiltinType` decltype specifier instead of a `MacroCall` |
| `_Nullable`/`_Nonnull`/… are not qualifiers, so an unnamed parameter takes the qualifier as its name | **confirmed, fixed** | `void f(int * _Nullable);` was a parameter **named** `_Nullable`; it now has no name (`Parameter` has no `NameExpr`) |
| `Feature::Atomic` is unused and `_Atomic` is a macro call | **confirmed, fixed** | `_Atomic(int)` is a `BuiltinType` with a `TypeId`; `_Atomic int` and `int * _Atomic` are specifier/qualifier. The rule is gated on `Feature::Atomic` |
| `_Complex`/`__complex__`/`_Imaginary`/`_Decimal*`/`_Float32x`/`_Float64x`/`__float80`/`__ibm128` are missing types | **confirmed, fixed** | gcc (MinGW, `-std=gnu2x`) accepts `_Complex`, `__complex__`, `_Decimal32/64/128`, `_Float32x/64x`, `__float80`; it rejects `_Imaginary` and `__ibm128`. The accepted spellings are `BuiltinType`s now, and the type reader joins the complex adjective with its operand: a file-scope `_Complex double z;` is one declaration whose `type_of` is `_Complex double`, and `double _Complex w;` records `double _Complex`. The rejected two stay out |
| `__label__` is not a keyword | **confirmed, fixed** | the scope builder binds each `__label__ done, again;` name as `BindingKind::Label`; the facts are `DeclKind::Other` with no type, the same kind a `done:` line produces |
| MSVC `__uuidof`/`__assume`/`__super`/`__leave` have no dedicated nodes | **confirmed, tolerated** | `__uuidof(p)` and `__super::f()` are in the MSVC parse table and parse as a call and a qualified name; the AST has no node of its own |
| MSVC `__asm` needs `(`/`{`, so the traditional `__asm mov eax, 1;` fails | **confirmed, fixed** | three errors before; `AsmStat` now, and `AsmBlock` covers MSVC as well as GNU |
| `__based`/`__sptr`/`__uptr` are missing | **confirmed, fixed** | all three are implementation keywords now; `__based(p)` keeps its parenthesised argument as a token group |
| `__has_builtin`/`__has_feature`/`__has_extension`/`__has_attribute`/`__has_declspec_attribute` answer `Unknown`, so the branch is skipped | **confirmed, fixed for GNU/Clang** | `CompilerFeatureProbe` writes the question as a two-line translation unit and runs the project's compiler; measured on a MinGW project, `#if __has_builtin(__builtin_expect)` and `#if __has_attribute(unused)` render `int yes; int attr_yes;`. `__has_feature` under gcc is a compiler error, so the probe returns `None` and the branch stays `Unknown` — the honest answer. MSVC's `cl` takes `/E`, so its path is left `Unknown` rather than asked wrongly |

What was changed, with the tests that pin it:

* `__alignof__` → `AlignofKeyword`, `__typeof_unqual__` → `DecltypeKeyword` (`cpp_lexer.rs`); the analysis spelling
  lists accept the new spelling too.
* `_Atomic` is read in both spellings and in the pointer/qualifier position, gated on `Feature::Atomic`; the
  declaration-start rule and the two macro-shaped specifier arms stand aside for it.
* Clang's nullability qualifiers are `ImplementationKeyword::Bare` — they disappear from the type/name positions
  instead of becoming a parameter name. `__nonnull` is read only when not followed by `(`, because glibc's
  `__nonnull((1))` is a macro.
* `_Decimal32/64/128`, `_Float32x/64x` and `__float80` are types for a GNU target, and a declaration-start anchor
  makes a compiler-spelled type begin a declaration.
* `AsmBlock` now covers both vendors; MSVC's traditional `__asm mov eax, 1;` consumes to its `;` as an `AsmStat`.
* **GNU nested functions** are read where the language has them: a block-scope definition is legal in a GNU **C**
  configuration, and the definition is a `Declaration` inside the enclosing body. A C++ parse keeps the old rule.
* **`__auto_type`** maps to `AutoKeyword`, so the declaration is a deduced-`builtin` specifier and the use-site
  deduction answers the initializer's type (measured: `__auto_type n = 1; n = 2;` hovers `int`).
* **`__based`/`__sptr`/`__uptr`** are read as bare implementation keywords; `__based(p)` consumes its group.
* **`_Complex`/`__complex__` are type spellings**, not a macro: the macro-standing-for-a-declaration rule stands
  aside for a type this compiler spells, so `_Complex double z;` at file scope is one declaration and its type is the
  two words together. Both written orders (`_Complex double`, `double _Complex`) are measured by
  `a_complex_type_keeps_the_adjective_in_its_spelling`.
* **GNU's `__label__` declares labels**, not variables: the scope builder binds each name in the declarator list as
  `BindingKind::Label`, the namespace `goto` and `done:` share, so the fact is `Other` and writes no type.
* **The `__has_*` family is asked of the compiler.** `CompilerFeatureProbe` (`cpp_toolchain/include/feature.rs`)
  renders the question as `#if __has_builtin(operand)` with two markers and runs the project's compiler over it; the
  answers are cached by operator, operand, language and standard, and the session passes the probe to the cook and to
  the unit render. Measured end to end: a MinGW project's `#if __has_builtin(__builtin_expect)` renders `int yes;`
  instead of `int no;`, and `__has_attribute(unused)` likewise. A compiler that does not have the operator, and MSVC
  (whose preprocessing flags the probe does not speak), stay `Unknown` rather than being answered wrongly.

Tests: `the_gnu_spellings_are_read_as_their_constructs`, `a_nested_function_is_a_declaration_inside_a_body`,
`atomic_is_a_type_specifier_and_nullability_is_not_a_name`, `msvc_asm_forms_are_statements`,
`a_gnu_label_declaration_binds_labels_not_variables` and `a_probe_answer_decides_the_branch`, plus the constructs
added to the three `extensions_*.rs` tables. The parser suite is 518 passed, 0 failed.

What is still open:

* **`_Imaginary`/`__ibm128`** — the measured compiler rejects them, so there is nothing to parse for this target; a
  PowerPC or another C implementation is a different measurement.
* **MSVC's `__has_*`** — `cl` takes `/E`/`/P`, and the probe is written for the GNU/Clang spelling; the operator stays
  `Unknown` in an MSVC project until that flag spelling is added and measured.
* **`__uuidof`/`__assume`/`__super`/`__leave`** — parse through the general call/name rules; a dedicated node is a
  later refinement, not a parse failure.

## A `_BitInt` bitfield struct had no layout — fixed

The hover on `struct bits` in `c-project/include/corpus.h` said:

```text
Layout — not computed: three, signed_wide, plain, other cannot be sized here
(a bitfield, a dependent type ... or the index does not hold).
```

Two independent refusals produced it:

* **A bitfield was withheld by rule**, because a declaration fact carries no width and an offset after one would be a
  guess. That is why `three`, `signed_wide` and `other` were named.
* **`_BitInt(N)` was not a type the layout reader knew**, so the non-bitfield `plain` was unsized too.

Measured with MinGW's gcc 15 under `-std=gnu2x`:

```text
sizeof(struct bits) = 6, _Alignof = 2
three        bits 0..2      (byte 0, a 1-byte unit)
signed_wide  bits 16..28    (byte 2, a 2-byte unit aligned to 2 — the type change starts a new unit)
plain        byte 4
other        bits 40..41    (byte 5, a 1-byte unit)
```

The sizes are measured too: `_BitInt(1..8)` is 1 byte, `9..16` 2, `17..32` 4, `33..64` 8, `65..128` 16,
`129..192` 24, `193..256` 32; alignment is the size up to eight, then eight. The implementation:

* `ClassLayout::of_members` places a bitfield in a storage unit the size of its declared type, aligned to that type;
  it shares the current unit only when the previous field had the same unit type and the bits still fit, and a normal
  member ends the unit. The measured cases are the doc comment and the tests (`Flags` 8 bytes, the `three`/`30`/`3`
  case 12 bytes, `struct bits` 6).
* `Session::a_bitfield_width` reads the `:` and a decimal literal at the member's own file offset; a macro or
  expression width (`: BITS`) still withholds the layout rather than guessing.
* `a_builtin_in_memory` sizes `_BitInt(N)` from the measured rule above.

Measured after: the real `corpus.h` card reads

```text
size 6 byte(s), alignment 2
  three        offset   0, 1 byte(s) (bits 0..3)
  signed_wide  offset   2, 2 byte(s) (bits 16..29)
  plain        offset   4, 1 byte(s)
  other        offset   5, 1 byte(s) (bits 40..42)
```

and `cpp_ls/tests/suite/hover_layout.rs` pins it.

Still not covered: **unnamed bitfields** (a fact has no name to ask about, so `unsigned : 0;` is invisible to this
walk), **`#pragma pack`**, and **MSVC's bitfield ABI** — a bitfield in an MSVC project is withheld rather than placed
by gcc's rule, because the two ABIs disagree on bitfields.
## A diagnostic in a branch nobody compiles — fixed

`include-not-found` was reported for an `#include` written inside a branch the compilation does not take. On neovim's
`os/fs.c`:

```text
#include <sys/acl.h>     inside #ifdef HAVE_ACL / #ifdef HAVE_SYS_ACL_H
#include <sys/access.h>
#include <sys/xattr.h>
#include <sys/uio.h>
```

The check did ask the guard layer (`the_guard_is_taken`), but `HAVE_ACL` is a name the macro walk does not know, so the
answer was `Unknown` and the finding survived. The cook had already decided the branch: its rendering contains no such
include, and `inactive` listed lines 24–30 as one of its dead regions.

**The filter is now central.** `CheckContext::push` drops any finding whose range starts inside one of the cook's
inactive spans (`Session::inactive_regions`), in the file's own coordinates. Every check goes through it, so no rule
can forget the question; `include-not-found`'s guard query stays as the cheaper first filter.

Measured on neovim (219 translation units):

```text
                                   before   after
include-not-found                      12       1
unused                                 12       0
macro-parentheses                      28      27
all findings                          433     409
syntax-error                            0       0
```

`os/fs.c` itself went from **5** findings (the four includes and a dead `unused` variable) to **0**. Live findings
were unchanged: `buffer.c` 2 unresolved calls, `window.c` 2, `tui.c` 2 assign-type-check, `api/vim.c` 3 — so the
filter is dropping dead-branch findings, not findings in general.

The spans may be **assumed** (`assumed_undefined` was 23 for `fs.c`): the cook applied C's "an undefined identifier
is 0" rule, and the UI's `inactive` answer says those spans must not be painted. Diagnostics take the stricter reading
— a finding about text the reading did not compile is not published — because a squiggle on a line the analysis did
not read is worse than silence. A live include (`#if 1`) is still reported, and the test that used to require an
undecided-guard finding now asserts silence once the cook has skipped the branch.