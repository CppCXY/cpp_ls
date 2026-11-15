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

So the trigger is a *combination* — the full `lazy_split_view` head (its base clause `public _Join_view_base<…>`, its
`_Iterator`/`_Sentinel` nested templates and their `requires` clauses) with the two constructors after it. The next
step is to halve the head with the same harness until the combination is a paragraph, then fix the parser: this is a
parser bug and not a ranges one, and it is the last thing between the niebloid and a typed pipeline.

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
