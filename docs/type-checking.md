# Type checking: what it does, what it does not, and the shape it should take

The reader's example, verbatim: `writeMessage(const std::string& json)` called with `writeMessage(123131)`. A compiler
refuses it. The analysis said nothing.

## Three holes, found by printing each step

The check has the form of a type check and did not work, and the reason was not one thing. Each step was measured in turn,
with a probe that printed the three answers for that one fixture:

```text
① the parameter `const std::string&` parses to `Reference { to: Named("std::string") }`, and `class_name()` answers `None`
   for a reference — so the index was never asked. FIXED: the check strips `const`/`&` before asking.
② `param-type-check` built its table from this file's function *definitions*, so a function **declared** in a header and
   called here was skipped before any comparison. FIXED: it falls back to the file's summary, then the index.
③ `a_class_can_be_built_from("std::string", int)` answered **`Some(true)`** for a class whose only constructor is
   `string(const char*)`. The cause was one line in `could_take_an_arithmetic_value`: it asked the same question of a
   **pointer** and a **reference**, so `const char*` was stripped to `char` and classified as arithmetic. A pointer is not
   an arithmetic value; a reference to one is. FIXED — `type_written.contains('*')` answers `Some(false)`, which is also
   what keeps `std::string(const char*)` from claiming it can be built from `123131`.

Still open, and narrowed: the reproduction **does not report yet**, and the probe placed between the class question and the
report never printed — so the route reaches the parameter comparison but not the class question, which means one of the
`continue`s before it fires. The next measurement is that boundary, not another rule.
```

The reproduction is `an_int_does_not_convert_to_a_string` in `tests/checks.rs`, `#[ignore]`d with that reason. Two of its
three obstacles are gone; the third is one function away.

## The shape it should take

Three checks ask the same question in three places — `assign-type-check`, `param-type-check`, `return-type-mismatch` — and
each assembles the answer itself, which is how one of them loses a step without the others noticing. The facility belongs in
`sema`, and the checks should only apply it:

```rust
// sema/convert.rs
pub enum Conversion {
    /// The same type.
    Same,
    /// A standard conversion: arithmetic widening, an array to a pointer, `T&` to `const T&`, a derived to a base.
    Implicit,
    /// A class target: the value has to build it, and the answer is the constructor set — the question
    /// `a_class_can_be_built_from` already answers.
    NeedsABuild,
    /// No conversion exists. **This is the only arm a check reports on.**
    Refused,
    /// Not this layer's to guess: an unread class, a template parameter, a dependent type.
    Unknown,
}

/// **Normalise, then compare once.** The normalisation is the part each check keeps getting wrong: strip references,
/// strip top-level `const`, look through `auto`, and only then ask. `Reference { to: Named }` and `Named` are the same
/// class question, and the check that asks `class_name()` without stripping finds neither.
pub fn conversion(from: &Type, to: &Type, context: &CheckContext<'_>) -> Conversion
```

Then:

```rust
// in each of the three checks
if conversion(&from, &target, context) == Conversion::Refused {
    report(context, …);
}
```

**Why this is worth doing rather than fixing the third hole only.** The three holes above were each found by measuring, and
each cost a probe and a round trip. With one facility, a hole is a hole in **one** function — and the next investigation
prints one thing instead of three.

## What must not be lost

The checks are quiet on `Unknown` on purpose. `Conversion::Unknown` is the honest arm for an unread class, a template
parameter, or a dependent type, and the record of why is in `docs/checks.md` and in each module's own documentation: a
diagnostic that fires on a type the analysis could not read is a diagnostic a reader learns to ignore.

## The fourth boundary, and where the next measurement goes

The `Unknown` arm of the comparison used to mean "someone else might accept it, stay silent", which is why a class target
was silent for every argument. It now asks the index inside that arm — with `strip_references` first, because
`class_name()` answers `None` for `Reference { to: Named(..) }` — and reports only on a definite refusal.

**Measured after that change**: the reproduction still does not report, and the trace shows the route reaches the comparison
with `convertible → Unknown`. So the answer the index gives for `a_class_can_be_built_from("std::string", int)` is still
`Some(true)`, even after `could_take_an_arithmetic_value` was fixed to answer `Some(false)` for a pointer. The next probe
belongs **inside** `a_class_can_be_built_from`: print the member list it reads (`members.own()`) and the answer for each
constructor. One of the two is not what the fixture wrote — either `members_of("std::string")` returns something other than
the one constructor, or the constructor's `parameter_list` is not `(const char* text)`.

That is one probe and one line, and the reproduction is waiting for it.

## Fixed: the `Unknown` arm asked nobody

The last boundary was the match arm itself. `check_the_arguments` compared the argument's type with each candidate's
parameter, and treated **`Unknown` as "someone might accept it"** — which is the right instinct for a relation that is merely
unsure, and the wrong one for a relation that is *structurally* unable to answer. `convertible_to` answers `Unknown` for
**every** class target, because it lives in the type algebra and never sees a constructor, so a parameter written
`const std::string&` was silent for every argument that reached it.

The arm now asks the index, with `strip_references` first (`class_name()` answers `None` for `Reference { to: Named(..) }`,
which is how the class question was never asked at all), and reports only on a **definite refusal**:

```text
std::string(const char*) called with 123131   →  ParamTypeCheck   ✓ (the reproduction, now a live test)
a class nobody has read                        →  Unknown          →  quiet
a template parameter                           →  Unknown          →  quiet
```

## What this cost, and the lesson worth keeping

Four boundaries, four probes, four fixes:

```text
① reference instead of the named type   class_name() → None         strip first
② declarations vs definitions           the call was skipped         fall back to summary, then index
③ pointer treated as arithmetic         const char* "takes" an int  a pointer is not a value
④ Unknown treated as "maybe accepted"   every class target silent   ask the index inside that arm
```

**And one silent failure worth naming**: the fix for ④ was applied twice without effect, because the patch anchored on a
comment block that the formatter had re-wrapped. What caught it was `strip_references` being reported as **never used** — a
warning, not an error. A helper with no caller is the cheapest signal that a patch did not land; it is worth building and
looking at before concluding that a rule is wrong.

## The facility exists now

`sema::convert` is the one question the checks ask:

```rust
pub enum Conversion { Same, Implicit, NeedsABuild, Refused, Unknown }
pub fn strip_references(parameter: &str) -> &str          // `const std::string&` → `std::string`
pub fn conversion(from: &Type, written: &str, ask_the_class: impl FnOnce(&str) -> Option<bool>) -> Conversion
```

`conversion` answers in one place, in this order:

```text
the type relation      Yes → Implicit, No → Refused, Unknown → keep going
the spelling           strip `const`/`&`, parse again, take the class name
the index              ask_the_class(class): Some(true) → NeedsABuild, Some(false) → Refused, None → Unknown
```

`Refused` is the only arm a check reports on, and `Unknown` is the arm that keeps the checks quiet about a class nobody has
read. **Two of the three checks use it now** — `param-type-check` and `assign-type-check` — and each is a `match` with one
arm. **All three use it now.** `return-type-mismatch` was the last: it compared with `Type::convertible_to` directly, and its own
module note claimed that was "the crate's single answer" — true before this facility existed, and the reason the class case
was missing from it. Each check is now a `match` on `conversion(..)` with one arm that reports, and the normalisation of the
spelling lives in one function.

## What the unification already bought

The class question had been answered **differently** in two checks before this: `assign-type-check` asked it about
`target.class_name()` — which is empty for a reference — and `param-type-check` did not ask it at all. After the move, a
fix to the normalisation reaches both, which is exactly how the fourth boundary was found: `strip_references` had no caller
until one place called it.

## `v[0]` — the last investigation, and what it found

The question was whether `operator[]`'s return type can be inferred. It can: the subscript arm exists, it distinguishes `[`
from `.` (the parser reads `v.size` as an `IndexExpr` too), the class name is normalised (`std::vector<int>` → `std::vector`),
and `members_of` substitutes the class's arguments into its members — the whole `T&` → `int&` chain.

What blocked it was one measurement:

```text
class vector { public: T& operator[](unsigned long index); };     members_of → []          ✗
class vector { public: T& at(unsigned long); T& operator[](…); };  members_of → ["at"]      ✗ (only the plain name)

the rendering contains both members; the summary contains only the class; the scope has one binding
```

**The member collector skips operator overloads whose names carry punctuation.** `operator[]`, `operator()`, `operator==` —
none of them become members, so the subscript arm finds nothing to return the type of. That is a scope/index fact, not a
type-inference gap, and it is the next thing to fix: the collector is where a declarator's name is read, and `operator[…`
is a name no declarator reader expects.