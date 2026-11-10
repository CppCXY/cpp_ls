# Open questions

Things a report from an editor pointed at, that are **not** fixed — with the measurement that says where they
stand, so the next run starts from evidence rather than from the report again.

## `std::endl` is not resolved at all in a real MSVC 18 project

Reported as "`std::endl` is not rendered as a function". Measured on `C:\Users\zc\Desktop\test_std`, a project whose
`endl.cpp` is `#include <iostream>` + `std::cout << std::endl;`:

* MSVC 18's `<ostream>` is a **shell**: it includes `__msvc_ostream.hpp`, and `endl` is declared there, at
  `__msvc_ostream.hpp:1007`.
* The index holds it, and correctly:

  ```text
  1007:51  endl   function returns `basic_ostream<_Elem, _Traits>` parameters `(basic_ostream<_Elem, _Traits>& _Ostr)` over [_Elem, _Traits]
  ```

* And the query does not see it:

  ```text
  $ inspect define --project test_std --file endl.cpp --needle "std::endl" --shift 6 --cook
  picked   unknown — NotDeclaredHere("std::endl")

  $ inspect hover … (same position)
  none
  ```

  That is **before and after** the declaring header's summary exists (`inspect facts --file __msvc_ostream.hpp`
  builds it; the answer does not change).

So this is **not** the classification this run fixed for macros (`6bbda4c8`): the fact is a function, in scope `std`.
It is the **visibility walk** — and that is now reproducible without the toolchain.

### Reproduced without the toolchain: two include hops are one too many

Three files under one project root:

```text
c.cpp   #include "b.h" + `void f() { std::endl(); }`
b.h     #include "a.h"
a.h     namespace std { void endl(); }
```

```text
$ inspect define --file c.cpp --needle "std::endl" --shift 6 --cook
picked   a.h:1:22   endl   function in std    ← one hop: c.cpp includes a.h

$ # the same, with b.h in between
picked   unknown — NotDeclaredHere("std::endl")   ← two hops

$ CPPLS_TRACE_VISIBILITY=1 inspect define …
[visibility] 2 file(s) after the order step, from …\c.cpp
```

**Two files** in both cases: the translation unit and its *direct* includes. `a.h` is never entered when it is
`b.h`'s include, so `std::endl` is invisible — and in MSVC 18 that is the ordinary case, because `endl` lives in
`__msvc_ostream.hpp`, one hop past `<ostream>`.

The visibility set is produced by `ProjectIndex::the_visible_walk` → `visible_files_with_modules` →
`macro_candidates(…, Collecting::Visibility(&mut seen), …)` (`index/project.rs`), so the descent that has to become
transitive is the macro walk's include crossing — including the state it carries, since a guarded edge past the first
hop is evaluated against an environment built by the files above it. The walk is measured in `docs/perf-progress.md`
(a cold closure 0.1 ms–3.4 s), which is why this was not rushed.

The next step is a probe inside `macro_candidates` at the point it pushes a child edge, on the three-file fixture:
the question is whether `b.h` is entered at all, or entered and not descended.


## An inlay hint rendering a deduced type expanded instead of the alias the source wrote

Reported with a screenshot: `auto newText = …` drawn as `basic_string<char, char_traits<char>, allocator<char>>`
rather than `std::string`. Five fixtures of the reported shape all print `std::string`:

* `auto text = make(1);` with `std::string make(int);`
* `auto text = h.name();` for a plain class's member
* …and for a class template's member (`Holder<int>::name()`)
* `auto text = r.get()->format(1);` through `Registry<Formatter>`
* the 39-header fixture's `main.cpp`, which has no `auto` at all

No `inspect inlay` answer in the real `clice` project prints `basic_string<` or `char_traits<` either. The expansion
therefore depends on something the screenshot does not show — a project alias, a macro, or a reading that was not
cooked. **A file and a line number are needed**, not another guess.
