# Usability: what a batch consumer has to know, and what it should not

Measured on `E:\EmmyLuaCodeStyle` (1216 C/C++ files), while building the style census
(`cargo run --release -p cpp_code_analysis --example style_probe -- E:\EmmyLuaCodeStyle`).

## The three queues, and the one nobody documents

```text
index_everything()              the index queue: parse, record facts, queue what each file includes
  advance(n)                    drains **the index queue only**  ← and answers "0 pending" while cooking has 1216
cooking                         a **second** queue: the readings that something asked for
  claim_cooking(n)              a slice
  cooking_materials(path)       the unit walk a reading needs
  render_them(materials)        the rendering
```

The tripping point, measured: after `add_project_files` + `index_everything` + one drain, the probe printed

```text
indexed in 160.5 s over 0 wave(s) — pending 0 / cooking 0
```

…and only **47 of 1216** files had a rendering, because the first version asked per file and stepped the pump with
`advance(1)`. That is 1216 single steps which cooked 46 files, and it took ten minutes. Driving the cooking queue directly
cooked **all 1216 in 56.3 s**.

## The numbers

```text
index      160.5 s   1216 files   132 ms/file
cooking     56.3 s   1216 files    46 ms/file
wall       222   s   (index + cooking + the census itself)
```

## The sharpest gap: `diagnostics` still answers for 47 of 1216

After every file was cooked, `Session::diagnostics(path)` returned `None` for all but the 47 that `Session::view(path)` also
had. So there is a **third** gate in front of a finding — the reading is held for the **open** documents, and a file whose
rendering was built on request is not one of them.

That is the usability problem this measurement was for: a batch consumer — a CI hook, a census, a project-wide lint — has
**no supported path** to "the diagnostics of every file". The server's pump is the only code that has one, and it is not
public. The shape of the fix is one call:

```rust
// session.rs, beside `diagnose_file`
pub fn diagnose_everything(&mut self, up_to: usize) -> Vec<(PathBuf, FileDiagnostics)>
    // drains both queues, renders what is wanted, and answers with everything — the loop the pump runs, as one method
```

Until then the census reports what it can see, which is why its counts are a **lower bound** and why the fixes it found
(cstyle-cast's preprocessor conditions, then its parameter lists) are worth more than its totals.

## What the census did find, before it was stopped

```text
cstyle-cast   813  →  #if (_MSC_VER), #if (ASIO_*) — preprocessor conditions: fixed, 376 left
              376  →  (const any_io_executor& e), (nullptr_t) — parameter lists: fixed
include-not-found  12   `# include <boost/config.hpp>`, `#   include <__config>` — a real gap in the fixture's include
                        path, not a false positive
```

Two false-positive classes, 813 + 376 findings, both found by running the rule on code nobody wrote for it and reading the
first three examples. That is what a census is for, and it is cheap — once a consumer can ask for a whole project.

## The batch API, and what it measured

`Session::diagnose_everything(up_to)` is the loop the pump runs, as one method: ask for every rendering, drain the index
queue, drain the **cooking** queue in slices (render each, **commit** each), then answer for every file the index has.

Measured on `E:\EmmyLuaCodeStyle`, 1216 files:

```text
index        159.7 s
cook + check 586.3 s   1191 files diagnosed      492 ms/file
wall         748.8 s
```

**The checks are the bottleneck, not the index.** 56 s of that 586 is cooking; the other ~530 s is the fourteen rules run
over every file — 445 ms/file of checks against 46 ms/file of cooking. That is the number to attack next, and it is worth
saying what it is not: it is not the parser (the same tree answers an inlay hint in milliseconds), and it is not the index
(160 s for the whole project).

## The full census

```text
cstyle-cast                17697      ✗ still over-reporting — see below
macro-parentheses           2455      ✗ to be read against its examples
use-override                 364      plausible
braces-around-statements     245      plausible
undefined-field              178      the earlier census's subject
include-not-found            143      boost and `__config` — a real include-path gap
argument-count               112      `_CopyFloatFromInt32((__int32)i)` — a cast argument misread
else-after-return            107      plausible
assign-type-check             80      `0` into a smart pointer — **valid C++**, a false positive
use-auto                      47      plausible
access-invisible              30      `data` on a boost type — to be checked
macro-redefined               27
implicit-bool-conversion      10      the type-driven rule, and the most conservative of the set
missing-return                 7
use-nullptr                    3
assignment-in-condition        1      `((op = …) != 0)` — deliberate, a false positive
```

Three findings about the **rules**, from reading three examples each — which is all a census needs:

- **`cstyle-cast` is still the worst.** 17,697 over 1191 files, and the report is a text scan; the classes left are template
  arguments and alias contexts (`(nullptr_t)`, `(Executor)`). A rule that needs to know "is this a *type* here" is a rule for
  the semantic model, not for a scanner — which is the same conclusion the first census produced for `implicit-bool-conversion`.
- **`assign-type-check` reports `0` into a smart pointer**, which is valid C++ (a null pointer constant) and a false positive
  worth fixing: the conversion check does not model the null-pointer-constant rule.
- **`assignment-in-condition` reports `((op = …) != 0)`**, the idiom the rule is written to allow.

## The checks are not uniformly slow — a few files are pathological

`examples/check_costs.rs` times `diagnostics` per file. On 300 files of `E:\EmmyLuaCodeStyle`:

```text
indexed in 0.8 s            (the same files, 300 of them)
total 64.7 s over 300 files — 215.6 ms/file average
median 0.6 ms   fastest 0.0 ms   slowest 45,283.7 ms
```

The average is a lie. **The median is 0.6 ms**, and the distribution's tail is the problem:

```text
timer_queue.hpp                45,283.7 ms    ✗ one file, forty-five seconds
deadline_timer_service.hpp      1,282.7 ms
win_iocp_io_context.hpp         1,013.3 ms
bind_handler.hpp …
~10 files                       800-1000 ms
every other file                under 10 ms
```

So the earlier "445 ms/file" figure from `diagnose_everything` was the same distortion over 1191 files: a handful of files
dominate the total, and the rules are cheap on ordinary code. That changes the target: not "make the checks faster", but
**find the path that makes one file take 45 seconds** — which is a quadratic, and a quadratic is a bug a user feels as a
freeze when they open the file.

The next measurement is a **rule sweep on one known-bad file**: `timer_queue.hpp` takes 45 s, so disabling rules in groups
(or one at a time) will name the rule in a handful of runs, and a 45 s file makes a wrong answer obvious immediately. The
two candidates from the code are `use-override` (a `members_of` per class, each of which walks the include graph) and
`implicit-bool-conversion` (`type_of` per condition) — both are index queries inside a per-node loop, which is the shape
every quadratic in this project has had.

## Which rule takes the forty-five seconds

`CPPLS_TIME_CHECKS=1` in `sema::check::run_check` prints one line per rule per file; summed over the same 300 files:

```text
AccessInvisible          30,067 ms   32 calls     ✗ half the total
ImplicitBoolConversion   11,108 ms   16 calls     ✗ the rule added this week
ParamTypeCheck            8,430 ms   27 calls
ArgumentCount             8,176 ms   42 calls
UndefinedField            3,916 ms   20 calls
AssignTypeCheck           3,271 ms   15 calls
MissingReturn                 2 ms    2 calls
CStyleCast                    1 ms    1 call
```

Two things follow, and neither of them says "make the rules faster":

**① Per call, not per rule.** `AccessInvisible` is 32 calls for 30 seconds — about **940 ms per call** on the few files where
it is slow, and 0 ms on the rest. The cost is not the rule's own walk; it is a query the rule makes.

**② The common factor is the model, not the checks.** Every expensive rule in that table asks `type_of` or `members_of`
inside a per-node loop. `AccessInvisible` asks for a member's visibility, `ImplicitBoolConversion` for a condition's type,
`ParamTypeCheck` and `ArgumentCount` for a callee's parameters — five different rules, one shape: an index query whose cost
depends on the **file's include closure**, called once per node.

So the next measurement is not another rule sweep. It is **timing `SemanticModel::type_of` and `members_of` themselves** on
`timer_queue.hpp` — the same function called from five rules cannot be the rules' fault, and a per-call cost that scales with
the include graph is a cache that is missing, not a query that is slow.

**The instrumentation is kept, gated by `CPPLS_TIME_CHECKS`**, because it is what found this and what will confirm the fix.
It prints only rules that took at least a millisecond, so an ordinary file logs nothing.

## The cache, measured — and what it did not fix

`members_of` was asked **64 times for one class** (`std::vector<heap_entry>`), each answer costing about **589 ms**. A memo
was added in `SemanticModel` (the same `RefCell<BTreeMap>` shape as the `types` memo beside it), and the file went from
45 s to 33 s:

```text
timer_queue.hpp   45,283 ms → 32,928 ms        the memo removed the repeats
members_of total  37,692 ms → 36,942 ms over 164 calls   the *distinct* classes are the cost
```

**So the repeats were not the problem — the first answer is.** 164 distinct classes at ~225 ms each is not a missing cache; it
is a query whose cost is wrong on the first call, and the memo cannot help that. The next measurement is inside
`direct_members`: it looks the class up in the scope tree, then in the index, and **then** — when both answer nothing — takes
the fallback described in its own comment ("the class spelling a use writes is not the key its facts are stored under") and
asks again through the include graph. On a file with a deep closure that fallback is the 225 ms, and it is being paid **per
class** rather than per file.

**What the memo is still worth**: it is correct, it removes 12 s from this file, and it makes the *second* question about a
class free — which is what the rules that ask repeatedly (five of them) needed. It stays.

## Where the 225 ms actually is

The fallback is two lines:

```rust
if let Known::Yes(resolved) = index.definition(without_arguments(class), path) {   // ← the 225 ms
    let qualified = resolved.fact.qualified_name();
    let found = index.declarations_in(&qualified, path);                          // ← a map lookup
```

The second is a lookup; the first **resolves a name** — the same query go-to-definition makes, and the one that walks the
include graph. It is not repeated per class any more (the `SemanticModel` memo covers that — 164 distinct classes are asked
once each), but it is paid **once per distinct class**, and a header that mentions 164 classes pays it 164 times.

So the cache that is still missing is **one level lower**: `ProjectIndex::definition(name, path)` is a pure function of the
index and the file, it is asked from five rules and from this fallback, and it has no memo. The measurement to make before
building one is its hit rate on `timer_queue.hpp`: if 164 classes resolve to 30 distinct names, a name-level memo removes most
of it; if they are 164 distinct names, the cost is the graph walk itself and the answer is a different one (the index already
knows which file declares what — `declarations_in` is that map — and the walk is looking up a name the index could key
directly).

## The two memos, and what is left

```text
before                         timer_queue.hpp  45,283 ms
SemanticModel::members memo    timer_queue.hpp  32,928 ms     −12.4 s
ProjectIndex::definition memo  timer_queue.hpp  31,930 ms      −1.0 s
```

The `definition` memo was worth one second, not thirty: its keys are the **files** as well as the names, and this file's 164
questions come from one file, so the collapse was from 164 to about ten names per *name* — which the model memo had already
done, one level up. It stays because it is correct and cheap (a `Mutex` behind a generation), and because it removes the
include-graph walk from every *other* file's questions about the same names.

**The remaining 32 s are inside `members_of_inner`** — 164 calls at ~200 ms each, and the memo cannot help a first answer.
The next measurement is its own sub-steps: the base walk calls `certainly_in_the_translation_unit` per level, which is an
include-graph walk, and that is the shape every expensive path in this project has had.