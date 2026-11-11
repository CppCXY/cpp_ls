# Open questions

Things a report from an editor pointed at, that are **not** fixed — with the measurement that says where they
stand, so the next run starts from evidence rather than from the report again.

## A constructor defined in the same file as its class is still drawn as a method

`EditorconfigPattern::EditorconfigPattern() { … }` in a `.cpp` was drawn as `function` — reported from an editor, and
fixed for the shape that matters: the class is declared in a **header**, so the definition's name is resolved through
the index (`IndexedKind::constructor`, pinned by
`semantic_token::a_constructors_qualified_definition_is_drawn_as_its_class`).

What is left is the other resolution path. When the class is declared in the **same file** as the definition, the name
is answered by that file's own *bindings*, and `BindingKind` records an out-of-class definition as `Function` rather
than `Constructor` — so `kind_of_binding`'s constructor arm never sees it. `BindingKind::Constructor | Destructor =>
NameKind::Type` is already there and already right; the missing half is `sema::scopes` binding an out-of-class
`Foo::Foo` with the constructor's kind. Measured: the one-file fixture answers `Some(Method)` where the two-file
fixture answers `Some(Type)`.

## `std::endl` drawn as a variable, i.e. with no semantic token at all

**Not reproduced.** Both halves of the question are now tests, and they answer the opposite of the report.

* **Resolution works through the path the server takes.** In a real MSVC 18 project — `#include <iostream>` and
  `std::cout << std::endl;` — asking for every project file's reading and letting the pump refine the closure answers
  the declaration:

  ```text
  $ cargo test -p cpp_code_analysis --test std_visibility_probe std_endl_resolves -- --ignored
  test result: ok
  ```

  That is `std_endl_resolves_through_the_servers_path`, an **ignored probe**: it needs the machine's own standard
  library and costs about six seconds, so it is run on demand rather than with the suite. The fact is a `function` in
  scope `std`, declared at `__msvc_ostream.hpp:1007` — MSVC 18 moved `endl` one hop past `<ostream>`, which is what
  made this look like a visibility problem.

* **The `NotDeclaredHere` this file used to record was an artefact of `inspect`.** `inspect` asks for **one** file's
  reading; a header two hops out then has no summary, so the visibility walk enters it and finds nothing to report.
  Measured on a three-file fixture (`c.cpp` → `b.h` → `a.h`): `inspect` answers `NotDeclaredHere`, while the same
  chain through the session's path answers the declaration —
  `reported_defects::a_declaration_two_includes_away_is_visible`.

* **The classifier was never the problem.** A namespace-scope function named by a qualified expression is drawn as
  `Function`/`Method`, never as a value — `a_qualified_function_is_drawn_as_a_function`.

So what would produce the report in a project where the name resolves is either a fact the classifier's `claimed`
set already holds (`_`-prefixed predefined macros, module declarations) or a region the cook dropped. Neither is
reproducible here. **The file it happened in is needed**, with the line.

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

## What this run did learn about `inspect` itself

A question asked by `inspect` is not always the question the server answers: it asks for one file's reading, so any
cross-file answer that depends on the **closure** — two hops of `#include`, a module's interface, a header behind a
`#include` a cook never read — can be wrong there while being right in the editor. The two fixtures above are the
minimal reproduction of that difference, and they are tests, so the next person to be misled by `inspect` can see it
in one run rather than in six rounds.
