# How macros are handled, and the one place two answers disagree

Written after a long chase of a single symptom — `std::endl` resolving sometimes and not others, then ten `auto`
declarations in MSVC's `<memory>` refused as `NotDeclaredHere`. Every claim below was measured; the commands that
measured it are named where they are used. Read this before changing anything about conditions.

---

## 1. The layers, and which one is authoritative

```text
  the file's text
      |
      |  preprocess(source, tokens)                          <- per file, no environment
      v
  FilePreprocessing { directives, macros: MacroTable, unclosed_guard, skipped_regions }
      |
      |  FileIndexer::index  (parses, builds scopes, builds facts)
      v
  FileSummary { declarations: Vec<DeclFact>, macros: Vec<MacroFact>, includes, guards }
      |                                                        ^
      |  the unit walk (TranslationUnit)                        | guards: FactGuard::Region(n)
      v                                                        | attached here, from the directives
  UnitState  — what a preprocessor's table would hold, in translation-unit order
      |
      |  cook_the_unit  ->  RenderedUnit  ->  render()
      v
  the rendering: macros expanded, directives resolved, ONE line
```

Two facts about this shape matter more than the rest:

* **A summary is built from the raw reading.** It records *every* `#define`, each with the region it was written
  in, and the regions are **not** evaluated when it is built. Evaluation happens later, in whichever consumer asks.
* **A rendering has no macros left.** Measured: `vcruntime.h` is 11444 bytes and renders to **524**; the rendering's
  macro table holds **0** bindings. `cargo run --release -p cpp_code_analysis --example render_macros -- <file>`.
  So a rendering cannot be the source of a macro environment — the raw reading is the only one there is.

---

## 2. One evaluator, eight environments

The evaluation of a condition is one implementation, and that part is right:

```text
  Branch::holds(macros)        guard.rs:79     #if / #ifdef / #ifndef / #elif / #else
  Region::visibility(macros)   guard.rs:175    the active branch of one #if, given the ones before it
  Guard::visibility(macros)    guard.rs:293    the chain of enclosing regions
```

What varies is the **environment** handed to it, and there are eight:

| environment | where | what it answers from |
|---|---|---|
| `Marked` | `graph.rs:963` | defined / undefined / **uncertain**, with bodies |
| `MacrosHere` | `environment.rs:86` | a seed `Marked` **plus** one file's own facts |
| `KnownMacros` | `environment.rs:271` | only definedness, from `MacroFacts` |
| `NoMacros` | `condition.rs:278` | nothing |
| `MacroTable` | `condition.rs:284` | one file's `#define`s by offset |
| **`UnitMacros`** | `summary.rs:1151` | the seed **plus the unit's own timeline** |
| `Live` | `cooked.rs:931` | the cook's running state |
| `PositionalMacros` | `mod.rs:130` | a `&dyn MacroBindings` at an offset |

**The cook and the unit walk use `UnitMacros`; every query uses `MacrosHere` over
`ProjectIndex::macros_at`.** Those two are built differently — the walk's from a live traversal, the query's from
the facts — and a guard decided by one can disagree with the same guard decided by the other.

Measured, and it was the whole of a round: `cppls-branch: Ifdef __cplusplus at 675 -> true` from the walk, while a
query over the same file answered as if it were false.

---

## 3. The defect: one guard, two verdicts

`ProjectIndex::macros_at` builds a `Marked` by walking facts in offset order and asking each one's guard. The
question goes to `ProjectIndex::include_visibility`, which is *not* the same function as the
`SummaryGuards::visibility_of` that every other consumer uses. Both walk `summary.guards.conditions_of(region)` and
call `Region::visibility` — so the *evaluation* agrees — but `include_visibility` adds one thing on top:

```rust
match holds {
    Some(true) => {}
    Some(false) => {
        let in_the_else = /* is `offset` inside some `#else` branch's body? */;
        if in_the_else {
            continue;                       // treat it as taken
        }
        return Visibility::Inactive;
    }
    None => unknown = true,
}
```

`region_at(at.region, offset)` chooses the branch **by offset**, and `Region::visibility`'s contract is already
"is the branch in force at this position the one that gets compiled". So `Some(false)` for a position inside an
`#else` means **that `#else` is not taken** — and the `continue` above turns it into *taken*.

That is what applies a dead `#define`. Measured on `vcruntime.h`:

```text
  fact at 7124  guard=Region(33)  Definition      #define _STL_LANG _MSVC_LANG     (inner #if)
  fact at 7244  guard=Region(33)  Definition      #define _STL_LANG __cplusplus    (inner #else)
  fact at 7422  guard=Region(32)  Definition      #define _STL_LANG 0L             (outer #else)
  fact at 8138  guard=Unconditional Undefinition  #undef _STL_LANG

  region 32 at 7422 -> active_branch=1 -> visibility Some(false)   <- correctly NOT taken
```

and the `0L` was applied anyway, so `#if _STL_LANG > 201402L` came out false, `_HAS_CXX17` and `_HAS_CXX20` came
out `0`, the `#if _HAS_CXX20` around `#include <atomic>` in `memory` was judged inactive, `_Locked_pointer` was
invisible, and ten `auto` declarations were refused. `cargo run --release -p cpp_code_analysis --example env_probe
-- <include dir> vcruntime.h 7500 _STL_LANG` prints all of it.

### Removing it is a net loss, measured

The obvious correction — delete the inversion — was tried and reverted:

```text
                         deduced   refused
  with the inversion        319       312      <- what ships
  without it                251       380      <- 68 fewer
```

`cargo run --release -p cpp_code_analysis --example types_probe -- target/scratch/headers.txt --limit 8 --cook`

The reason is that the inversion is **blunt but load-bearing**: where a condition is *undecidable*, `None` makes the
walk skip the fact, and the inversion turns that into "taken". Most of the corpus is undecidable, so the blunt rule
keeps far more than it wrongly kills. **The correction is not to delete it — it is to make `region_at` and
`Region::visibility` answer `Some(...)` where they now answer `None`.** Until that is done, deleting it is a
regression. The code carries this measurement in a comment so the next reader does not repeat the experiment.

---

## 4. What is *not* wrong

Each of these was suspected during the chase and cleared by measurement:

* **Preprocessing and macro expansion.** `vcruntime.h` renders 11444 → 524 bytes; nested macros are rescanned
  (`_STD_BEGIN` → `_EXTERN_CXX_WORKAROUND namespace std {` → `extern "C++" {`), with a recursion guard and a depth
  budget. `CPPLS_TRACE_MACRO=<name>` prints each layer's answer.
* **The macro environment.** `Marked` and the seed both answer `__cplusplus -> Defined`, at the offset in
  question. `index.macros_at` is not "losing" a name the seed has.
* **The guards on the facts.** They are `Region(32)` / `Region(33)`, not `Unconditional` — the region information
  survives into the summary intact.
* **`region_at` / `branch_at`.** `active_branch=1` for the `#else`, which is right.
* **`SummaryGuards::visibility_of`.** It answers `Some(false)` → `Inactive` for that same region at that same
  offset, with no inversion. **That function is the correct one**, and the defect is that the walk does not use it.

---

## 5. The probes

Every step above came from one of these, and none came from reading alone.

| probe | the question |
|---|---|
| `render_macros` | what a rendering holds — macros, directives, sizes |
| `env_probe` | one name, asked of `Marked` and the seed, **plus every branch's verdict and the facts' guards** |
| `seed_probe` | what a macro environment says at a given offset |
| `region_probe` | which regions a file's text decides dead |
| `dump_tree` | what the grammar made of this text, nodes and tokens |
| `lookup_probe`, `member_probe_at`, `closure_probe` | the name, member and include layers |
| `types_probe` | the main metric, with a reason per refusal |
| `fact_scopes` | 34070 facts over 93 headers in **10 s**, against 229 s through `types_probe` |

---

## 6. The rule this leaves

**One question, one implementation.** In this layer the evaluation is already single; the *environments* are eight,
and two of them answer the same guard differently. The next change here should make the walk call
`SummaryGuards::visibility_of` — the function the rest of the crate trusts — rather than carry a private variant of
it, and should replace the blunt `#else` inversion with a decidable answer from that function.

The criterion for that work, unchanged and cheap:

```text
  macros_at(vcruntime.h, 7500)  _STL_LANG   must be 202400L      now 0L
  visible_files(memory)         contains atomic?                 now false
```

`cargo run --release -p cpp_code_analysis --example seed_probe -- "<MSVC include dir>"`

When it passes, `_HAS_CXX17` becomes 1, `_HAS_CXX20` becomes 1, `atomic` joins `memory`'s visible set, and the ten
`auto` refusals should turn over with it — one fix, ten numbers.
