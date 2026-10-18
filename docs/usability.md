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