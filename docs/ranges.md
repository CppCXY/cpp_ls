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
compiler would. A fixture for this work therefore needs a compile-database entry with `/std:c++latest` (or
`[compile] standard = "c++23"`), and that is the first thing to check before reading any of the numbers below.

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
