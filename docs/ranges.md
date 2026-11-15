# `<ranges>`: what works, what does not, and why

The question: `std::views::` completes, but a **pipeline's result** cannot be inferred — no type on
`auto r = v | std::views::filter(f)`, no inlay hint for it, and a lambda written `[](auto x) { … }` leaves `x`
unknown.

The fixture (`target/bench/case_ranges/main.cpp`):

```cpp
#include <ranges>
#include <vector>
#include <string>

struct Item { int id; std::string name; };

int main() {
    std::vector<Item> items;
    auto ids = items | std::views::filter([](const Item& item) { return item.id > 0; });
    auto names = items | std::views::transform([](auto item) { return item.name; });
    auto squares = std::views::iota(1, 10) | std::views::transform([](auto n) { return n * n; });
}
```

## First check the standard, not the machinery

Before any of this is worth debugging, the file has to be a **C++20** translation unit: MSVC's whole `<ranges>` sits
inside `#if _HAS_CXX20`, and when the configuration does not supply C++20 the *cooked rendering of the header is the
fallback stub* — measured with `CPPLS_DUMP_RENDERING_DIR` on a project with no compile database:

```text
#pragma once
_Pragma("message( ... \"warning \" \"STL4038\" \": \" \"The contents of <ranges> are available only with C++20 or
later.\" )")static_assert(true,"");
```

206 bytes. There is no `std::views` in that program, so `NotDeclaredHere("std::views")` was **right**, and a project
without `/std:c++20` (MSVC's own default is C++14) cannot use `<ranges>` — the analysis is reporting what the
compiler would.

**Two defects were behind that, and both are fixed.**

1. **The compile database's standard was never read when it was spelled MSVC's way.** `config_from_arguments` knew
   `-std=` and not `/std:`, and `LanguageConfig::from_flag` had the same gap — so a CMake MSVC project, whose every
   entry says `/std:c++latest`, was analysed as the compiler's default. `cplusplus_value` also had no number for
   `latest`, which is the word MSVC actually writes. Both are read now, and `c++latest` means `202302L` — the newest
   *settled* value the table has.
2. **A header with no command of its own had no standard.** The measured consequence is the 12-byte stub above: the
   *project* stated C++20 in its database, and `<ranges>` — a system header no entry names — was cooked with no
   standard at all. A file with no command now inherits the project's standard **when the entries agree**
   (`CompileCommands::a_unanimous_standard`); a project whose entries disagree keeps answering `None`, because then a
   header really is built two ways.

Measured after both, on the fixture whose `compile_commands.json` names `main.cpp` with `/std:c++latest`:

```text
<ranges> rendering        12 bytes  ->  217 875 bytes (live session) / 1 803 132 (cooked as its own root)
std::views::filter        NotDeclaredHere  ->  filter variable in std::ranges::views type `_Filter_fn`
```

**The seed needed it too.** A per-file configuration was not enough: `compilation_environment` builds the macro table
every cook starts from, and that table is project-wide — so a header's *own* config could be right while the seed it
was rendered against had no `__cplusplus` at all. Measured by dumping the live session's rendering of `<ranges>`:
**12 bytes** before the seed carried the standard, **217 875 bytes** after. With it:

```text
std::views:: adaptors  20 item(s)  (all, as_const, common, counted, drop, ...)
hover on transform     `inline constexpr _Transform_fn transform` - `std::ranges::views::transform`, a variable of
                       type `_Transform_fn`, declared at ranges:2504:52   (was NotDeclaredHere)
```

The raw reading of `<ranges>` is still thin — parsing the header's own text gives 612 declarations and loses
everything from about line 2490 (`transform`, `take_view`, `drop_view`, `zip_view`, `common_view` are all absent),
while its **rendering** parses to 22 345 declarations with `transform` at `std::ranges::views`. That is the
preprocessor doing its job: the raw text is full of `_STD_BEGIN`/`_RANGES`/`_EXPORT_STD`, and only the rendering is
the program a compiler reads. So the declaration, the class (`_Transform_fn` with its two `operator()`) and
`std::views` all resolve now.

**What is left is the type of the niebloid and the call through it.** `typeDefinition` on `transform` answers **0
locations**, so the variable's type `_Transform_fn` is not resolved from the importing file's context even though the
hover card names it, and `type_of(std::views::transform)` answers `UnknownType`.

The measurements narrow it to one name. Plain-qualified spellings work, alias-qualified ones do not:

```text
auto hop5 = std::ranges::views::iota;   _Iota_fn        (the spelling the facts carry)
auto hop2 = std::views::iota;           UnknownType     (the spelling the header lets a reader write)
```

and the reason is visible with `CPPLS_TRACE_TYPES=1`, which prints the facts each prefix of the name resolves to:

```text
[alias] "std::views" candidates:
  ... "views" scope Some("std::ranges") kind Namespace type Some("ranges::views") @ ranges
```

The trailing **`_EXPORT_STD namespace views = ranges::views;` lands one namespace too deep** in the cooked reading: its
fact is `std::ranges::views` with target `ranges::views`, so the qualified name `std::views` names nothing and the
adaptors cannot be reached through the alias. Completion after `std::views::` still works because it follows the alias
fact's *target* rather than its qualified name — which is why the two questions disagree about the same spelling.

**Where the extra scope comes from: a `template … requires …` declaration that never closes.** The rendering's own
text is **balanced** — a comment/string-aware scan of the 204 KB `<ranges>` body ends at depth 0, and every
`namespace views {` block sits at the depth `std` + `ranges` asks for. What is wrong is the *parse*: the ancestor
chain of a `namespace views {` token near the end of the header is

```text
NamespaceDecl "namespace views { struct _Slide_fn …"
Declaration    "template <input_range _Vw, forward_range _Pat> requires (vie…"
Declaration    "template <input_range _Vw, forward_range _Pat> requires view…"
CompoundStat    (the body of `namespace ranges`)
```

— the namespace is nested **inside two declarations**, so a `template … requires …` written before it never closed
and swallowed the rest of the file, including the alias at 7666. The last 7 141 bytes of the rendering (the `_Zip_fn`
block to EOF) parse correctly on their own, with the alias in `std`, which is what says the swallowing starts earlier.

**A reproduction, and how far it is shrunk.** Extracting `lazy_split_view`'s declaration (10 294 bytes, offsets
108 422–118 716 of the rendering's `<ranges>` body) and wrapping it as

```cpp
namespace std { namespace ranges {
    <the declaration>
}; } }            // the closers the file has
namespace probe { int marker; }
```

parses with `marker` in **`std::probe`** — the declaration swallows its own closers, so one scope is left open. That
is the bug in 10 KB. Shrinking it further, with the same wrapper and the same question:

* the class's first 8 470 bytes plus `};` are **clean** (marker at global scope);
* adding the two constructors after them — the `constexpr lazy_split_view(_Vw, _Pat) noexcept(…) : _Range(…), _Pattern(…)
  {}` pair, the second one `template <input_range _Rng> requires …` — makes it nested;
* either constructor alone is clean, and so are the two constructors in a synthetic class;
* the whole class head is needed for the flip: 5 of its 6 top-level parts plus the pair stay clean, 6 plus the pair
  is nested.

So the trigger is a *combination* — the class head of `lazy_split_view` with two constructors after it — and it is
now **922 bytes**:

```text
template <input_range _Vw, forward_range _Pat> requires (…) class lazy_split_view
    : public _Lazy_split_view_base<_Vw, _Pat> {
    constexpr lazy_split_view(_Vw, _Pat) noexcept( is_nothrow_move_constructible_v<_Vw>&& is_nothrow_move_constructible_v<_Pat>)
        : _Range(::std::move(_Range_)), _Pattern(::std::move(_Pattern_)) {}
    template <input_range _Rng>
        requires constructible_from<_Vw, views::all_t<_Rng>>
              && constructible_from<_Pat, single_view<range_value_t<_Rng>>>
    constexpr lazy_split_view(_Rng&&, range_value_t<_Rng> _Elem)
        noexcept(noexcept(_Vw(views::all(::std::forward<_Rng>(_Range_))))
              && noexcept(_Pat(views::single(::std::move(_Elem)))))
        : _Range(views::all(::std::forward<_Rng>(_Range_))), _Pattern(views::single(::std::move(_Elem))) {}
};
// then: namespace probe { int marker; }  ->  parsed as std::probe, i.e. one scope left open
```

The body of the class is **not** needed — the head plus the two constructors is the whole reproduction — and the
reduction says which part of the second constructor matters:

| change | verdict |
| --- | --- |
| (unchanged) | nested |
| `requires constructible_from<…> && constructible_from<…>` → `requires true` | **global** |
| `single_view<range_value_t<_Rng>>>` → `bool` | **global** |
| `views::all_t<_Rng>` → `bool` | **global** |
| either `noexcept(…)` simplified | nested |
| all four simplifications | global |

So it is the **constraint** of the converting constructor — specifically its template-ids (`views::all_t<_Rng>>`,
`single_view<range_value_t<_Rng>>>`) — that the parser fails to close, and the declaration swallows the rest of the
file. A minimal input built from that shape alone (`requires c2<V, holder<R>> && c2<P, holder<holder<R>>>`) does not
reproduce, so the class head's own `requires` clause and the base clause are part of the trigger as well.

Delta-debugging the fixture (deleting a phrase, re-testing, keeping every deletion the bug survives) brings it to
**450 bytes**, and the tree at the class's own closing brace says what happens:

```text
tree at 21:80                       (inside the constructor's member-initializer list)
  Syntax(CallExpr)      "views::single(::std::move(_Elem))"
  Syntax(ArgumentList)  "(views::single(::std::move(_Elem)))"
  Syntax(Initializer)   "(views::single(::std::move(_Elem)))"
  Syntax(Declarator)    "_Pattern(views::single(::std::move(_Elem)))"     ← a *declarator*
  Syntax(Declaration)   "template <input_range _Rng> requires constructible_f…"
  Syntax(ClassBody)     "{ constexpr lazy_split_view(…) …"
```

**The member-initializer list is being read as a second declarator.** The conversion's `: _Range(…), _Pattern(…)` is
parsed as `_Pattern(…)` — a declarator like the `b` of `int a, b;` — and since it is followed by `{}` it becomes a
function definition whose body is the rest of the class. That is why the class's `};` closes the *constructor* and
every later `}` closes one scope too few.

Which parts are needed, by deletion and replacement:

| change | verdict |
| --- | --- |
| `::std::forward<_Rng>(_Range_)` → unqualified `fwd(_Range_)` | **global** (the qualification is part of it) |
| `noexcept(noexcept(…))` → `noexcept(true)` | **global** |
| the member-initializer's content → `_Range(0), _Pattern(0)` | nested (content does not matter) |
| the class-level `requires` clause, the base clause, the first constructor | nested (all deletable) |
| the second constructor's `requires` clause | global |

So the shape is a constructor with a **`requires` clause followed by a nested `noexcept` whose operand is a
functional cast of a qualified call**, and then a member-initializer list. The `:` of that list is not recognised,
and the list becomes declarators.

**Fixed.** `decls.rs` asked the parser's *live* "the last declarator was a function" flag at the `:` — and the
suffixes in between parse expressions, one of which contains a declarator of its own, which resets the flag. The arm
now uses the answer taken **when the declarator was read** (`declarator_is_function`), which is the same question at
the right time and at least as permissive (it is the live flag or a parameter list in the event stream, so an empty
parameter list is still recognised). Measured:

```text
the reproduction            marker in std::probe  ->  marker in probe
std::views::iota  (hop2)    UnknownType            ->  _Iota_fn
std::views::transform (hop) UnknownType            ->  _Transform_fn
`, the trailing alias       std::ranges::views     ->  reaches `std::views` (the adaptors resolve)
```

The regression test is `a_constrained_constructor_keeps_its_member_initializer_list` in
`crates/cpp_parser/tests/concepts.rs`, over the versioned fixture; it fails without the fix (the parse leaves an
`ErrorNode`) and passes with it.

**What is left of step (3)** — typing the niebloid's *call*: `std::views::filter` has a type now, but
`std::views::filter([](const Item& item) { … })` does not, so `items | std::views::filter(…)` still has no type.

The call arm already exists (`index/project/calls.rs`, "a call of an object"): it types the callee, checks the type
is a `Named` class, and asks `operator_returns_of(…, "operator()")` for what the operator returns. The measurement
says where it stops: MSVC's niebloids write

```cpp
template <class _Pr>
constexpr auto operator()(_Pr&& _Pred) const noexcept(…)
{ return _Range_closure<_Filter_fn, decay_t<_Pr>>{…}; }
```

— the return type is the **placeholder** `auto`, and `operator_returns_of` answers the *written* `returns`, which for
this member is that keyword. `parse_type_spelling("auto")` cannot be `_Range_closure<…>`.

So the next change has a fixed shape, and the machinery to copy is in the same file:

| need | where it already exists |
| --- | --- |
| is a return spelling a placeholder (`auto`)? | `is_a_placeholder` (used at `type_of.rs:2298`) |
| the declared return, when it is not a placeholder | `declared_returns_of_with` (`declarations.rs:2890`) |
| the body's return with the call's arguments substituted | `deduced_returns_of_substituted` (`declarations.rs:1485`) |
| a worked example of both, for a *function* callee | the `NamedDeclaration::Indexed` arm of `what_a_call_has_in` (`type_of.rs:2338`) |

**Done, and pinned by a unit test that needs no header at all** —

```cpp
struct closure { int value; };
struct functor { auto operator()(int x) const { return closure{x}; } };
functor make_one;
void use() { auto c = make_one(1); }        // `c` was Unknown, and is `closure` now
```

`operator_returns_of` (in `type_of.rs`) now treats a member whose recorded return is the placeholder `auto` — or no
return at all — as a body to read: `deduced_returns_of_the_member` reads the declaring file through the `declaring`
closure, finds the operator's declaration and calls `deduced_returns_of_substituted`. Two details cost the most:

* **A fact's offset is a *file* offset and the tree speaks the *reading*.** The translation
  (`reading_offset_of`) and the fallback to the raw offset are the ones the `Indexed` arm of `what_a_call_has_in`
  already makes; `definition_at` at the un-translated offset finds nothing.
* **An operator's binding is not where a name's is.** `definition_at` answers for a *name*, and `operator()` is a
  keyword and a pair of parentheses, so a probe on its ten bytes finds no binding at all. What the deduction needs
  from a binding is only the offset `declaration_of` starts its walk from and the kind, so the nearest binding is
  borrowed and re-pointed at this declaration — with the fact's own name, which is the one thing that must not be
  guessed.

The test is `a_call_of_an_object_deduces_an_auto_returning_operator` in `session/tests/types.rs`; it failed with
`Unknown(UnknownType("make_one(1)"))` before this change and asserts `closure` after it.

**The argument's type is part of the answer.** `_Filter_fn::operator()` returns
`_Range_closure<_Filter_fn, decay_t<_Pr>>`, so the body's spelling names the *argument's* type, and a deduction that
does not know what the call passed answers `wrapper<Pred>` — measured, and a red test before this step. Two things
were needed:

* `operator_returns_of` gained a sibling, `operator_returns_of_with_arguments`, which takes the call's argument
  types; the old name delegates with an empty list, so the six other call sites are untouched, and the object arm in
  `calls.rs` is the one that passes them.
* the deduced spelling is then substituted **twice**: the operator's own parameters from the argument types
  (`deduce_the_parameters` with the fact's written parameter list), and the class's from the type the call was made
  on. The deduction answers with what the file wrote, so the first substitution is what turns `wrapper<Pred>` into
  `wrapper<int>`.

Three tests now pin the shape from three directions: the same-file functor
(`a_call_of_an_object_deduces_an_auto_returning_operator`), the header-and-`extern` version
(`a_call_of_an_object_in_another_file_deduces_the_operator`), the namespace-alias-and-`inline constexpr` version
(`an_adaptor_behind_a_namespace_alias_deduces_its_operator`), and the parameter-naming return
(`an_auto_returning_operator_naming_its_argument_is_deduced`).

**And one step further, which is not done.** A return that puts an *alias template* between the parameter and the
argument — `<ranges>`'s own `_Range_closure<_Filter_fn, decay_t<_Pr>>` — is deduced as
`range_closure<_Filter_fn, decay_t<int>>`: the parameter is substituted, the alias is not resolved. Measured as a
test that is not committed (it would be red), and the next step for this stage: resolve an alias template **inside**
a deduced spelling, which is the same walk `a_trait_alias_resolved` already makes when the whole spelling is one.

**One shape was found that the deduction gets *wrong*, not merely empty**, and it is the shape `<ranges>` writes:

```cpp
struct _Filter_fn {
    template <class Rng, class Pred> auto operator()(Rng&& rng, Pred&& pred) const { return view{}; }
    template <class Pred>            auto operator()(Pred&& pred) const { return range_closure<_Filter_fn, Pred>{}; }
};
auto c = filter([](int v) { return v > 0; });   // deduced `view` — the *two*-argument overload
```

`member_fact` answers the **first** declaration of the name, and the first is the two-argument form; passing the
argument count through to it (tried) does not change the choice — it needs the member list, not the single-fact
query. **Fixed**, and the two details that were not obvious:

* the overload is chosen by the **number of arguments the call wrote** (`crate::inlay::arguments_of(call).len()`),
  not by the number of argument *types*: a lambda's type is unnamed, so a call that passes one lambda can have an
  empty type list, and the arity then looked like zero.
* a member list's `declared_in` is **not a path** — measured, it is the class's name (`"_Filter_fn"`). The member's
  body is read from the file the *class* is declared in, which is where it is written.

The member's return is now read through one function, `the_members_return`, shared by the first-declaration path and
the arity-matched path, so the two cannot disagree. The test `an_overloaded_adaptor_deduces_the_operator_that_the_
arguments_fit` asserts the one-argument call's closure rather than the first overload's `view`, and it passed.

**A test for the combination, and what the live server says now.** The two halves above are covered separately; the
combination `<ranges>` writes — an alias template in the return *and* a lambda argument — is now
`an_alias_in_the_return_survives_a_lambda_argument`, and it passes.

The live server's own trace finally became readable, and not through `CPPLS_TRACE_TYPES`: that probe produced
nothing there while `CPPLS_TRACE_COOKING` printed, so the instrumentation was re-gated on `cooking()` to be seen at
all. Two measurements from it:

* `commit_a_cooked: 0 declaration(s)` is **not** a statement that the summary is empty — in one run almost every
  header reported 0 (`xstring`, `xutility`, `new`, …) while `arm64_neon.h` reported 3 041, and the live hover of
  `std::views::filter` resolves. What the number counts is narrower than "declarations in the summary".
* with the deduction instrumented, the live answer was `[member-auto] no binding for "operator()" at 80164` — the
  member fact is found, its offset is a *file* offset, and `definition_at` finds no declaration named `operator()`
  there. Translating that offset into the tree's coordinates (the change the `Indexed` arm already makes) removed
  even that message: the deduction is then **not reached at all**, which moves the question one step earlier — to
  whether `members_of`/`member_fact` answer for `_Filter_fn` in the live session, and with what parameter counts.

**What is still open in this stage:** the `<ranges>` fixture itself. Two leads from the live trace, both concrete:

* `members_of` **is** answered there:
  `[arity] _Filter_fn wanted "operator()" arity 1: 2 member(s) ["operator() count=Some(2)", "operator() count=Some(1)"]`
  — so the overload selection has the right input and picks the one-argument form.
* the server reads `<ranges>` **as part of a program**: its own log (`%LOCALAPPDATA%\cpp_ls\logs\<project>.log`)
  says `read a program as one piece: 96 file(s), 374 683 token(s), 0 error(s)`. That is a *different* reading from
  the per-file rendering this round's offsets were being translated against, and it is the strongest candidate for
  why a fact's offset (`80 164`) finds no `operator()` in the file's tree: the fact may be an offset of the *unit*,
  not of the file.

That is the next measurement: which reading the fact came from, and what tree its offset belongs to.

**And the failure is not the lambda's.** A second call shape was written into the fixture and measured in the same
way — `auto a_closure_int = std::views::filter(1);` — and it fails exactly like the lambda's:

```text
auto a_closure_int = std::views::filter(1);          could not be typed
auto a_closure     = std::views::filter([](int v)…)  could not be typed
```

So the live failure of this stage is the **call arm for `<ranges>`'s adaptor generally**, not the unnamed lambda
type, and every unit test of the call arm passes.

**The cause, measured.** With the trace finally readable end to end, the live answer is:

```text
[type_of] depth  1  "std::views::filter(1)"
[type_of] depth  0  "std::views::filter"
[call-arm] "std::views::filter(1)" callee resolved=Variable
[class-file] _Filter_fn class_file=Some("…/include/ranges") arguments=1
[members-return] operator() at SourceRange { start_offset: 80164, length: 10 } returns=None file=…/include/ranges
[member-auto] view …/include/ranges tree 12 bytes          ← the STL4038 stub
```

The **view** the deduction is handed for `<ranges>` is **12 bytes** — the `#else` stub the whole stage started with —
while the file's *cooked rendering* (the one the server dumped while indexing) is 217 875 bytes. The cook and the
view disagree about the same header: the cook runs with the project's C++20 standard, and the view a query asks for
is built without it. So the name resolution that hover uses (`_Filter_fn`, from the *cooked summary*) succeeds, and
the body the deduction tries to read is 12 bytes long — `#pragma once` and nothing else.

**The two readings differ in *which unit* they came from, and that is the last thing in this stage.**

One correction to the numbers above, made in a later round and worth writing down: the 12 bytes and the 217 875 bytes
were produced by **different walks**. The big one is the reading the *program* walk makes — it is what the cooked
*summary* holds, and the `std::ranges::views` scopes in the facts are its evidence (`_STD_BEGIN` was expanded). The
12-byte one is what a **per-file** walk produces for a header: a unit rooted at the header itself, an environment
without the includer's defines, `_HAS_CXX20` false, and MSVC's `#else` body — `#pragma once` and a `static_assert`.

A query's view is built from that per-file reading (`Session::view` reads the rendering cache, and
`fact_from_the_rendering`/the cook fill it from `rendering_of`), which is why a name resolves from the program's
summary while the body is 12 bytes. Stopping the one writer that could put a self-included rendering into the cache
was tried and measured: the view still read 12 bytes, so the per-file cook puts it there too.

**That is the next fix:** a header's rendering must be made from a unit that **includes** it — the program's — not
from a unit rooted at the header. Every step after this depends on it: the adaptor's body, `operator|`, and the
lambda's parameter types.

The code already knows how to choose that unit: `Session::the_unit_that_reads` walks the include graph's reverse edges
(shallowest includer that can be a program root) and `cooking_materials` uses it — but only when the designated
root's timeline is **held in memory and not provisional**. When it is not, the cook falls back to a **composed
environment** (`the_indexed_environment` → `configuration_from_environment_and`).

**Measured, and that fallback is what cooks `<ranges>`:**

```text
[materials] …/case_ranges/probe2.cpp   a timeline answers
[materials] …/include/ranges           COMPOSED environment
```

And the composed table is built from the closure's **macro facts only** — `the_indexed_environment` walks the include
closure backwards and takes each `#define` the summaries recorded, with no seed from the compilation itself. The
compiler's predefined macros are exactly the ones no file in the closure defines: `__cplusplus`, `_MSVC_LANG`, and
with them `_HAS_CXX20`. So the composed rendering of `<ranges>` is its `#else` branch — the 12 bytes.

**That is the fix, and it is one function:** seed the composed environment with the compilation's predefined macros
(the table `compilation_environment` builds, which the walk already starts from), keeping the closure's own
definitions as the overrides they are. Everything after this stage depends on it. There the callee is `std::views::filter`, and
whether the call arm reaches `operator_returns_of` depends on the *cooked* `<ranges>` facts being in the index — the
live server has them (its hovers resolve `std::views::transform`), a reloaded cache does not, because the summary on
disk was built by the previous parser. So the next measurement is the live server's own answer for
`items | std::views::filter(…)` after a cache-clear, and after that `operator|`.

## What it answers today

## What it answers today

```text
type of `items`            std::vector<Item>                     ✓
type of `ids`              UnknownType("ids")
hover on `ids`' declaration `ids(…)` — a function, declared in a function body
define on `std::views`      NotDeclaredHere("std::views")
inlay hints                 12:63 type -> `bool` (the lambda's deduced return); nothing for `ids`
```

## The first blocker is not the pipe, and not the niebloid

The **tree is right**: `Declaration → InitDeclarator → Initializer → BinaryExpr(items | CallExpr(filter, LambdaExpr))`,
with the lambda's own `ParameterList` and body inside it. The **fact is wrong**: `ids` is recorded as
`kind: Function` with no parameters, so hover prints a function signature and the type query has nothing to stand
on. That is the same shape `build_facts` uses for a *function definition* — a declarator with parameters and a body
— and a lambda inside an initializer matches it.

So a pipeline's type cannot be inferred for a reason that has nothing to do with `<ranges>`: the variable is not
recorded as a variable.

**Fixed.** The scope builder asked "does this declarator contain a parameter list" of its whole subtree, and an
initializer is an expression — a lambda inside it declares nothing about the declarator it initializes. The question
now skips initializer subtrees (`a_parameter_list_of_the_declarator`), and the fixture reads:

```text
12:10  ids      variable type `auto`        (was: function)
13:10  names    variable type `auto`        (was: function)
hover on `ids`  `ids` — a variable of type `auto`, declared in a function body   (was: `ids(…)` — a function)
```

The type is still `auto` — that is steps (2)–(5) below, which is where the range machinery actually begins. A
regression test pins the two halves: the lambda-in-initializer declaration is a `Variable`, and `void f(int)` is
still a `Function` (`a_lambda_in_an_initializer_does_not_make_the_declaration_a_function`).

## Why the rest still needs work, in order

1. **`std::views` does not resolve by name.** The index holds `namespace views in ranges` (seven of them in MSVC's
   `<ranges>`) and `variable filter in ranges::views` — the `std::` prefix is lost on the raw reading, and the
   alias `namespace views = ranges::views;` is not among the facts. A qualified lookup for `std::views::filter`
   therefore answers `NotDeclaredHere("std::views")` even with `--cook`. Needed: the alias recorded as a fact, and
   the index's spelling of the namespace chain (`std::ranges::views`) from the cooked reading.
2. **A niebloid is a variable whose type is callable.** MSVC ships `inline constexpr _Filter_fn filter;` where
   `_Filter_fn` is a **struct** with an `operator()` — exactly the "struct + lazy constructor" technique the question
   names — and GCC does the same with `_Filter`/`__filter_fn`. The facts already carry
   `filter variable in ranges::views type _Filter_fn`, so once (1) resolves, the analysis must (a) give the variable's
   type, and (b) call `operator()` on an **object**, which is a call whose callee is a member of the object's class
   taking the lambda. The call machinery exists (`what_a_call_has`, overload resolution); what has never been
   exercised is a call through a *variable of a class type*.
3. **`operator|` is a free template** in `<ranges>`: `template <class _Rng, class _Closure> … operator|(_Rng&&,
   _Closure&&)` constrained on both sides being a range / a range-adaptor closure. Its return is the closure applied
   to the range: `_Closure::operator()(_Rng)`, which for `filter` is `_Filter_fn::operator()(vector&, lambda)` →
   `filter_view<vector<Item>, lambda>`. So (3) needs the free-function deduction it already has, *plus* the
   dependent return type resolved through the closure's `operator()` — a chain the engine reaches only if (2) works.
4. **A lambda's `auto` parameters come from the invoker.** `[](auto n) { return n * n; }` has no type of its own;
   the adaptor calls it with `*iterator` (for `transform_view`, `_Fn{}(*_It)`), so the parameter's type is the
   range's `reference`. Nothing in this analysis binds a lambda to the call site that invokes it. That is a feature
   of its own: "which callable does this lambda become an argument of, and what does that parameter receive". It
   needs (2) and (3) to say what the adaptor is, and then the parameter of the adaptor's `operator()` is the type to
   give the lambda's `auto` — with `const Item&` from the vector's reference, and `int` for `iota`'s value type.
5. **Inlay hints and hover then follow.** `auto ids = …` shows its type once (1) and (3) are in place; `auto n`
   inside a lambda shows once (4) is.

## What is worth doing first, measured

| Step | What it unblocks | Size |
| --- | --- | --- |
| (1) an initializer makes a variable | `auto r = <pipeline>` is a variable at all; hover stops calling it a function; its type can be asked | small, and it is a defect independent of `<ranges>` |
| (2) the `std::views` alias and the namespace chain | `std::views::filter` resolves; completion already works, so the facts are there to name | small–medium |
| (3) a call through a class-type object (niebloid) | `views::filter(f)` has a type; without it nothing else can | medium |
| (4) `operator|` on the closure | `v | f` has a type (`filter_view<vector<Item>, lambda>`) | medium, depends on (3) |
| (5) lambda parameters from the invoker | `[](auto n)` gets `int`; inlay hints inside lambda bodies | large |

The measurement that matters for the order: **(1) is a defect that has nothing to do with ranges** — a variable
containing a lambda is misclassified everywhere — and it is the cheapest. (2) is next because it is a fact the
index holds and loses. (3) and (4) are the range machinery, and (5) is the largest and the least useful until the
first four are right.
