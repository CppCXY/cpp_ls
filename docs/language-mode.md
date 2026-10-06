# The `_HAS_CXX17` chain: three defects, one symptom

Written after the symptom `_HAS_CXX17 == 0` was chased to its end and fixed. Every claim here was measured, and the
commands that measured it are named. Read this with `docs/macros.md`, which describes the layers this sits in.

**The criterion, and its authority.** `vcruntime.h` writes its own language-mode macro, and the **compiler's own
answer** is what says whether we agree with it:

```text
  cl /nologo /Zc:preprocessor /Zc:__cplusplus /PD /std:c++latest empty.cpp
      #define _MSVC_LANG 202002L   #define __cplusplus 202002L      (/std:c++20)
      #define _MSVC_LANG 202400L   #define __cplusplus 202400L      (/std:c++latest)
```

`_STL_LANG` is **defined by the header** (`vcruntime.h:262/264/267`) and `#undef`'d at line 302 — the compiler never
defines it. With `/Zc:__cplusplus` the two values are equal in **every** standard, so `_MSVC_LANG > __cplusplus` is
always false, the `__cplusplus` branch is always the one taken, and the `0L` branch at line 267 is **unreachable by
construction**. Two of the three defects below are exactly "we took it anyway".

```text
  cargo run --release -p cpp_code_analysis --example seed_probe -- "<MSVC include dir>"
      _STL_LANG = 0L          ->  must be a value >= 201402
      _HAS_CXX17 UNDEFINED    ->  must be 1
      _HAS_CXX20 UNDEFINED    ->  must be 1
```

---

## 1. `verdicts` was keyed by region, and the answer depends on the position

`ProjectIndex::macro_candidates` walks a file's facts in offset order and asks each one's guard whether it was
compiled. The answers are memoised, and the memo's own note says why that is right: a region should be decided once,
at the first point the walk can ask, because asking again later would read the region's own body as evidence about
its own condition.

The key was `region`. The answer is not a function of the region: `region_at(at.region, offset)` chooses the
**branch** by the position being asked about, and `Region::visibility` answers "is the branch in force *here* the one
that gets compiled". So one region has several right answers, one per branch.

Measured on `vcruntime.h`:

```text
  fact at 7124  guard=Region(33)  #define _STL_LANG _MSVC_LANG    (inside the inner #if)
  fact at 7244  guard=Region(33)  #define _STL_LANG __cplusplus   (inside the inner #else)
  fact at 7422  guard=Region(32)  #define _STL_LANG 0L            (inside the outer #else)
```

`conditions_of(33)` is `[33, 32, …]`, so asking about 7124 **also** records a verdict for `Region(32)` — computed
where 7124 stands, inside the `#if`, which is `Some(true)`. The dead `0L` at 7422 then reads that verdict back and
is applied. `_HAS_CXX17` came out `0`.

**Fixed** by keying the memo by `(region, active_branch)`. "Is branch *b* of region *r* compiled" depends on the
branches before *b* and on nothing else, so it does not move as the walk advances.

## 2. An `#else` branch was inverted on top of the verdict

`include_visibility` had this:

```rust
Some(false) => {
    let in_the_else = /* is `offset` inside some `#else` body? */;
    if in_the_else { continue; }        // treat it as taken
    return Visibility::Inactive;
}
```

`Some(false)` already means "the branch in force here is not compiled", so for a position inside an `#else` it means
**the `#else` is not taken** — and the `continue` turns it into taken.

It was written for `_STL_COMPILER_PREPROCESSOR`, whose defining branch was being judged "not taken". **That was
defect 1**: the verdict read back was the one recorded for a fact inside the `#if`. Removing the inversion was tried
once and reverted, because the main metric fell from 319 deduced to 251 — **a measurement of defect 1, taken before
defect 1 was fixed**. With the memo keyed correctly, removing it moves the metric by nothing and fixes the `0L`.

The lesson is worth more than the fix: two changes were made at once and one number was read.

## 3. A macro whose body is one *name* could not be read

`vcruntime.h` writes `#define _STL_LANG __cplusplus` — an **alias** — and then asks `#if _STL_LANG > 201402L`.
Reading that needs the alias followed to a value.

A summary could not say it. `MacroFact` stores a body as a `cpp_parser::MacroBody`, which is a **shape**
(`Specifier`/`Statement`/`Block`/…), not tokens, plus a range into the file the `#define` was written in — and
`MacroFact::value` held only "the body is one integer literal". So `#define X Y` was indistinguishable from
`#define X`.

**Fixed** by recording the alias where the replacement list *is* in hand: a new `MacroFact::alias`, set by
`macro_alias` when the body is exactly one identifier. Three readers were taught to use it:

* `MacrosHere::alias_of` follows it by name, with a depth cap so `#define A B` beside `#define B A` stops;
* `Marked::observe` builds a `MacroDef` whose body is the alias as an **identifier token**, so the ordinary
  expander walks the chain (`_STL_LANG` → `__cplusplus` → `202400L`) one link at a time;
* the summary codec round-trips it, so a cached summary does not lose it.

---

## What the three are worth

```text
                            deduced   refused
  before                      319       312
  after                       340       291
```

`cargo run --release -p cpp_code_analysis --example types_probe -- target/scratch/headers.txt --limit 8 --cook`

and `_HAS_CXX17` / `_HAS_CXX20` are `1`, which is what the compiler says they are.

## What is left, and it is one feature

The refusals are now dominated by a single shape — **a name that depends on a template parameter**:

```text
  24  UnknownType("::std::_Get_unwrapped(_First)")      `decltype(auto)` template, body is the argument
  21  UnknownType("::std::_Get_unwrapped(_Last)")
  17  NotDeclaredHere("_VbIt::_Myoff")                  `_VbIt` is a template *parameter*, not a class
  17  NotDeclaredHere("_VbIt::_Myptr")
  11  UnknownType("::std::_To_address")
   8  UnknownType("::std::_To_address(_First)")
```

Both halves are the same missing step: **substituting the enclosing template's parameters**. `_VbIt::_Myoff` cannot
be looked up until `_VbIt` is bound to a real type, and `_Get_unwrapped(x)` cannot be typed until `_Iter` is — at
which point its body (`return static_cast<_Iter&&>(_It);` on the branch that is taken) gives the argument's own type.
Roughly 84 of the 291 refusals are in these two families.

`decltype(auto)` currently answers `None` on purpose (`sema/declarations.rs`, "a deduced return type is not a type
this layer can name"), and that is the honest answer until the substitution exists.
