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
It is the resolution path — either visibility across the two include hops (`endl.cpp` → `<iostream>` → `<ostream>` →
`__msvc_ostream.hpp`) or the qualified-name query. The next step is to trace `visible_declarations` for that path
with a header that is two hops out, which MSVC 18's `__msvc_*` split makes ordinary.

A fixture that reproduces the shape without the toolchain is three files: `c.cpp` includes `b.h`, `b.h` includes
`a.h`, `a.h` declares `namespace std { void endl(); }`.

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
