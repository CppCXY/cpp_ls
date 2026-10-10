# The parse defect under the diagnostics cost

The diagnostics channel's cost is not its checks and not its caching — it is that **inference cannot resolve the
symbols in these headers**, so every variable pays a full 47–54 ms walk to be told nothing. Measured on MSVC's
`<vector>`: 1491 calls to `type_of_expression`, each 47–54 ms, 3537 ms of one request.

This note is the **upstream** half of that: what the parser cannot read. It records one construct, isolated to a
two-line reproducer, and states plainly which parts are proven and which are still a hypothesis.

---

## 1. The evidence that points here

```text
<memory>     2 parse error(s)
<vector>     4 parse error(s)
<xutility>  39 parse error(s)
```

Concentrated, not diffuse — which is the whole reason to look:

```text
  xutility, 39 errors:
     16x  line 724   expected primary expression
     10x  line 724   expected `}, but get identifier
      8x  line 733   expected `;` after expression
      5x  line 1122  expected `}
```

Two lines account for 34 of the 39. A file with 39 *different* problems is a file with 39 fixes; a file with 34
errors on two lines is **one construct the grammar cannot read**, repeated.

## 2. The construct, isolated

`xutility:722-726`:

```cpp
template <class _Ty>
    requires _Dereferenceable<_Ty> && requires(_Ty& __t) {
        { _RANGES iter_move(__t) } -> _Can_reference;      // line 724 — 26 of the 39
    }
using iter_rvalue_reference_t = decltype(_RANGES iter_move(_STD declval<_Ty&>()));   // line 726 — parses FINE
```

`_RANGES` is `::std::ranges::` (`yvals_core.h:1387`). The **same spelling parses on line 726 and fails on 724**,
and the difference is the braces: line 724 is a **compound requirement**, `{ expr } -> Constraint`.

Reduced to one failing line, with each variant measured:

```text
  concept X = requires(T& t) { { f(t) }      -> int; };      0 errors
  concept X = requires(T& t) { { NS f(t) }   -> int; };      2 errors   ← the defect
  concept X = requires(T& t) { { NS::f(t) }  -> int; };      0 errors
  concept X = requires(T& t) { { ::ns:: f(t) } -> int; };    0 errors
  concept X = requires(T& t) { { *p }        -> int; };      0 errors
  concept X = requires(T& t) { { p->f() }    -> int; };      0 errors
  concept X = requires(T& t) { { NS x }      -> int; };      2 errors   ← same shape, no call
  concept X = requires(T& t) { { f(t) + 1 }  -> int; };      0 errors
```

**The trigger is two adjacent identifiers.** And the same two tokens are accepted everywhere else — which is what
makes this a seam rather than a rule:

```text
  as a SIMPLE requirement:   concept X = requires(T& t) { NS f(t); };     0 errors
  as a statement in a body:  void g() { NS f(t) }                         0 errors
  inside a compound `{ }`:   concept X = requires(T& t) { { NS f(t) } … }  2 errors
```

The two error nodes reported are the compound requirement's own closing brace and a stray one after it, so what
happens is that the requirement ends early and the `}` that was meant to close it has nothing left to close —
the same *symptom* the `noexcept` seam produced, whose fix is documented at `exprs.rs:parse_requirement_expression`.

## 3. Why this matters beyond one construct

The two identifiers are `_RANGES iter_move`, and in real C++ they are **not two identifiers at all** — after
preprocessing they are `::std::ranges::iter_move`. So this is the raw/cooked seam again:

```text
  the file writes      { _RANGES iter_move(__t) } -> _Can_reference;
  a compiler sees      { ::std::ranges:: iter_move(__t) } -> _Can_reference;
```

A *raw* reading is reading text a compiler never sees, and `NS f(t)` is genuinely not an expression — so a raw
reading cannot be blamed for refusing it. The defect is that **the refusal is not contained**: the requirement ends
early, the tree loses the enclosing declaration, and 26 diagnostics follow from one line.

## 4. What is proven, and what is not

**Proven.** The trigger is two adjacent identifiers inside a compound requirement; the minimal reproducer above
reproduces it with no macros, no headers and no MSVC; the same tokens are accepted as a simple requirement and as
a statement.

**Not proven.** That this construct is what produces `<xutility>`'s 34 errors. Two attempts to test it failed for
reasons worth recording, because they are traps:

```text
  "define the macro and re-parse"
      Void: the parser reads *tokens*. Prepending `#define _RANGES ::std::ranges::` adds a
      directive to the tree and changes no token below it. Measured: 84 errors either way.
  "compare the raw and cooked readings"
      Void on this path: `view_of_the_rendering` returned none — no rendering was built for the
      file — so there was nothing to compare. The cooked reading is the one that would answer
      this, and obtaining one for a single header is the next step.
```

The honest next step is therefore the cooked reading, not more grammar reading: render `<xutility>` with the
closure's macros and count its errors. `examples/align_preprocessor.rs` already renders a file, and the same
machinery answers this in one run. **If the cooked reading parses clean, the fix is in the raw reading's recovery**
(contain the failure to the requirement); **if it does not, the fix is in the requirement rule itself**.

## 5. The instrument

`examples/parse_errors.rs` prints a file's errors grouped by message with the count and the first line, which is
how the two-line concentration was found:

```text
cargo run --release --example parse_errors -p cpp_code_analysis -- <project-dir> <file>
```

It is the tool the next round wants: per-file, grouped, with the first line — because the finding here was not
"39 errors", it was "34 of them are two lines".
