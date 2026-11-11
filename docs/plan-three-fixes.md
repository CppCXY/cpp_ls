# Three fixes, specified before they are written

The rule for all three: **the tree decides every question about this file, and the index decides the cross-file one it
was built for.** A text peek is a workaround; so is "the class the cursor happens to be in" when the declaration says
which class it is. Each item is a fixture first, then the change, then the workspace suite and clippy, one commit.

## 2. A member is identified by its declaration, not by where the cursor is

**Today**: `name_level_offsets` narrows with `the_class_the_cursor_is_in` — the innermost class scope around the
*cursor*. The class a *use* is written in is not the class it refers to: `diagnosticBuilder.SpellCheck(...)` inside
`LuaCodeFormat::NameStyleCheck` is written in `LuaCodeFormat` and names a member of `DiagnosticBuilder`, so the filter
lets it through and Find References mixes two classes' members.

**The rule**: a member use is attributed by the **type of its object**; a class-body use by the **enclosing class**; any
other use by its qualified name. That is C++'s lookup, and each of the three cases is a node:

| occurrence | node | the answer comes from |
|---|---|---|
| `obj.member`, `p->member` | `CppMemberExpr` (object + name) | the object's type — `a_member_of_an_instance` / `member_across_files` |
| `member` inside a class body or a member function | the scope chain | that class's members |
| anything else | `CppNameExpr` | qualified-name lookup, as today |

**Work to do**

1. `cpp_code_analysis`: one public query — `member_references(index, files, class_qualified, member_name, budget)` —
   whose occurrence filter is the table above, reusing the index's member lookup rather than re-deriving types here.
   The target's class comes from the target fact's `qualified_name()`, never from the cursor.
2. `cpp_ls`: `name_level_offsets` asks for the symbol by **fact** (as `symbol_references_of` already does) and hands the
   query the fact's qualified name; `the_class_the_cursor_is_in` stops being an input to attribution.
3. Fixture: two classes with a same-named member, a call on each, plus a call on an instance of the *other* class from
   inside the first — Find References, rename and the CodeLens count all answer 1 use, not 3.

**Why not keep the cursor's class**: it is a question about the use site, and the answer is a property of the
declaration. It is also why the CodeLens and Find References disagreed: the lens had the fact and the list did not.

## 1. A name in a member position is a field, and its binding is the AST's name

**Today**: `: _supportNonStandardSymbol(false)` draws that name as a function. Two separate questions are being answered by
one rule: *is this a call?* (the initialiser list looks like one) and *what is this name?* (a member of the class being
constructed).

**The rule**: the declarator's `name` is the last segment of the AST's name node — `qualified_name()` already splits it —
never a `rsplit("::")` on the declarator's text, and a name in a mem-initialiser is bound as a **field of the enclosing
class**, because that is what [class.base.init] says it names.

**Work to do**

1. `sema/scopes.rs`: `is_a_constructor_declarator` and the constructor/field binding name both read `qualified_name()`
   segments from the tree instead of `declarator.text()`. (The `X::X` rule then also handles `X< T >::X` and a newline
   before `::` for free.)
2. Bind each mem-initialiser's name in the class scope as a `Field`/`Variable` binding when the class declares one, so
   the classifier's step 1 finds a name in that position at all.
3. Fixture: the constructor with `: _support(false)` — the qualifier is `Type`, the initialiser's name is a
   `Property`/member, and an ordinary call in the same list is still a `Function`.

## 5. A class template's member types are substituted, not guessed

**Today**: `auto &result = results.emplace_back();` leaves `result` unknown. `emplace_back`'s return type `reference` is a
member alias of `std::vector<T>` — a **dependent** type, and this layer substitutes function-template parameters only.

**The rule**: an alias declared in a class template is substituted with the template's arguments when the class
instantiation is known: `vector<LuaDiagnosticInfo>::reference` → `value_type&` → `LuaDiagnosticInfo&`.

**Work to do**

1. `index/project.rs`: when a call's callee is a member of an instantiated class (`results` has type
   `std::vector<LuaDiagnosticInfo>`), look the member's declared return type up **through the class's own aliases**
   (`using reference = value_type&;`) with the class arguments substituted; one function, `member_return_type`, that
   takes the instantiated type rather than a spelling.
2. The substitution itself reuses `substitute` (the function-template path), because the operation is the same one: a
   declared text with parameters, and an environment that maps them.
3. Fixture: `std::vector<int> v; auto &r = v.emplace_back(); r = 3;` — `r` is `int&`; and the reported shape
   `results.emplace_back().Start` resolves `Start` through `LuaDiagnosticInfo`.

**What it is not**: a table of member names per container. The alias chain is in the class's own facts; a project class
with `using reference = T&;` must answer the same way, and a table would only answer for the library.

### Round 1 finding (single file is already right)

Springfield: the single-file fixture above **passes** — `name_level_offsets` answers 0 places for `Format::SpellCheck`
even with `builder.SpellCheck(...)` in the same file, so the "class the cursor is in" filter is not what lets the other
class's member through when everything is in one file. The reported case has the two classes in **different headers**, and
the call inside the first class's *inline* body in its own header, while the target is defined in a `.cpp`. The next
fixture is therefore cross-file:

```text
builder.h   struct Builder { int SpellCheck(int t) { return t; } };
format.h    struct Format { int NameStyleCheck(Builder&, int); int SpellCheck(int); };
format.cpp  #include "format.h"  int Format::NameStyleCheck(Builder &builder, int style) { return builder.SpellCheck(style); }
```

Ask for the uses of `Format::SpellCheck` (declared in `format.h`) and assert that `format.cpp`'s `builder.SpellCheck` is
**not** one of them. If the cross-file fixture also passes, the report is about a shape neither fixture has — and the next
step is to reproduce it from the editor's own file:line, not to change the filter.
## Progress

- **Item 1 — done, two commits.** `NameKind::Property` exists and a variable whose binding's *own* scope is a class is a
  property (asking the scope chain turned every parameter into one — measured), on both the binding and the index paths;
  the fixture is `a_mem_initialiser_names_a_field` (`: _support(false)`). `is_a_constructor_declarator` no longer reads
  the declarator's text: it takes the two segments from `qualified_declarator_name`.
- **Item 2 — both fixtures pass today** (single file and cross-file). Recorded above: the reported case has a shape
  neither fixture reproduces, so the next step is the editor's file:line, not a change to the filter.
- **Item 5 — not started.** The plan is unchanged.
### Round 3: the attribution is already by the declaration

A third fixture asks from a **use** rather than from a declaration:

```cpp
struct Builder { int SpellCheck(int t) { return t; } };
struct Format {
    int SpellCheck(int t) { return t; }
    int Run(Builder &b) { return b.SpellCheck(1) + this->SpellCheck(2); }
};
```

Cursor on `b.SpellCheck`, and the answer is **1 place** — not `this->SpellCheck`, which belongs to `Format`. So the
three shapes that can be built (single file, cross file, cursor on a use) all attribute a member by its declaration
today: `definition_at` on a member access resolves through the index's member lookup, and the occurrence filter compares
classes already. **Item 2 has no reproducible defect**; what it needs is the editor's `file:line` for the report that
started it (a lens said 1, Find All References showed `DiagnosticBuilder::SpellCheck`), not another change.
### Round 4: the simple alias chain already works

The fixture `a_container_members_alias_is_substituted_with_its_element_type` passes today: with

```cpp
template <class T> class vector {
public:
    using value_type = T;
    using reference = value_type&;
    reference emplace_back();
};
std::vector<int> values;
auto &result = values.emplace_back();   // int&
```

the answer is `int&`, so the substitution walks the chain (`reference` → `value_type&` → `T&`) and is not limited to one
step. What the reported case has that this fixture does not is the **standard library's own chain**: MSVC's `vector`
declares `using reference = _Alty&;`, and `_Alty` is itself an alias of a traits class's `value_type`. The next fixture
therefore writes a chain with a **traits class**:

```cpp
template <class T> struct allocator_traits { using value_type = T; };
template <class T> class vector {
public:
    using value_type = T;
    using reference = allocator_traits<value_type>::value_type&;   // a dependent member of a class template
    reference emplace_back();
};
```

If that passes too, the remaining difference is the macros in the real headers, and the next step is a real project
measurement (`inspect type --project … --file CodeFormatCore/src/.../LuaCodeFormat.cpp --needle result`), not another
simplified fixture.
### Round 5: the mechanism is right — the class is not there

The dependent-alias fixture passes (`reference = allocator_traits<value_type>::value_type&` → `int&`), so the
substitution walks a chain that crosses into another class template. The real project then gave the answer, and it is a
different question:

```text
$ inspect hover --project EmmyLuaCodeStyle --file CodeFormatLib/src/LuaCodeFormat.cpp \
    --needle "auto &result = results.emplace_back" --shift 7 --cook
`result` — a variable, declared in a function body
The type is deduced from the initializer, and that could not be typed: the type of
`std::vector<LuaDiagnostic>&` is not known here, so what its members are is not known either.
```

So `emplace_back` is not the problem: **`std::vector` itself has no entry**. The index summarizes the project's files, and
a system header is read only as text while a file is cooked — no facts, so no class, so no member, so no alias to
substitute. The fixture passed because its `vector` was a project declaration.

**That changes item 5.** The honest fix is not "substitute the alias better" but "answer a member of a class the index
does not hold from the **rendering**, which has the class's body inlined". The rendering is one translation unit with the
standard library in it, and this crate already parses it for every cooked file; the missing piece is a lookup of a
member's declaration **in that tree's class scopes**, with the class arguments substituted — the same substitution the
fixtures above prove works, pointed at a tree instead of at facts.

Design, before writing it:

1. `FileView` gains one query: the class definition named `X` **in the rendering**, plus its argument list from the
   instantiation the caller holds (`std::vector<LuaDiagnostic>`).
2. `member_return_type` takes the class's `CppClassDecl` (from the tree) instead of the index fact when the index has
   none, and substitutes the class arguments into the member's return text — the operation the fixtures already exercise.
3. Fixture: a **header that is not indexed** (a second project directory the session never adds to its file list) whose
   class the cooking inlines; the member's alias must still answer. That is the shape a system header has, without the
   standard library in the test.
### Round 6: the rendering path already answers an unindexed header

`a_class_the_index_does_not_hold_is_answered_from_the_rendering` passes: with `lib.h` **included but not a project
file** — the shape of a system header — `auto &result = values.emplace_back()` still answers `int&`. So the "answer it from
the rendering" design written above is **already what happens**, and the real project's failure is specific to the
standard library's own text, not to where the class is looked up.

The next step is therefore a trace of one sentence, not a design: the hover said

```text
the type of `std::vector<LuaDiagnostic>&` is not known here
```

That sentence is produced by one condition in this crate. `grep -rn "is not known here" crates/` names it, and the
condition it guards is the thing to measure on that file (`inspect type` on the declaration of `results`, then on
`emplace_back`). Only then is there a change to make.
### Round 7: `std::vector` is known and has 993 declarations — and offers no members

Three measurements on `CodeFormatLib/src/LuaCodeFormat.cpp:304`:

```text
$ inspect hover … --needle "auto &result = results.emplace_back" --shift 7
`result` — the type of `std::vector<LuaDiagnostic>&` is not known here

$ inspect complete … --needle "results.emplace_back" --shift 8
complete at 304:32: scope `std::vector`, prefix ``, 0 item(s)

$ inspect summary --file …/MSVC/…/include/vector
summary of …/include/vector: 993 declaration(s), 20 macro fact(s), 6 include(s) (6 resolved)
```

So the third line rules out "the system header is not indexed": it **is**, with 993 declarations. The second line is the
actual failing step — the scope is recognised (`std::vector`) and **no members are offered**, so the instantiated class
is resolved as a name but its members are not found. That points at the member query for an **instantiated class
template** (`std::vector<LuaDiagnostic>` → the facts whose scope is `std::vector`), not at the alias substitution, not at
the index's coverage, and not at the rendering.

The next step is one query: what does `inspect members` answer for `std::vector` **without** the argument list, and with
it. If the bare name answers 993-derived members and the instantiated one answers none, the fix is in the member lookup's
template-argument handling — a real defect, not a boundary.
### Round 8: the `members` probe cannot answer — it says `NotDeclaredHere` for a project class too

```text
$ inspect members --class "std::vector" … --cook
members of `std::vector`: unknown — NotDeclaredHere("vector")

$ inspect members --class "LuaDiagnostic" … --cook     (a class of the project itself)
members of `LuaDiagnostic`: unknown — NotDeclaredHere("LuaDiagnostic")
```

Both unknown, including a class the file uses — so this subcommand is **not** a probe of member lookup: it resolves the
class name by a path that answers "not declared here" (`NotDeclaredHere`) regardless. It cannot be used to decide whether
`vector`'s members are reachable, and nothing should be concluded from it.

What *is* established, from the completion that answered a **resolved** scope:

```text
complete at 304:32: scope `std::vector`, prefix ``, 0 item(s)
```

The member query for `std::vector<LuaDiagnostic>` recognises the class and offers nothing. The next instrument is not
another `inspect` subcommand but a **unit fixture** that reproduces "instantiated class template in another header, whose
members are looked up by the bare class name" — the same shape, without the standard library — and the first thing to
check in it is whether the class's members are found at all when the class is a **template** rather than a plain struct.

If plain `struct` answers and `template <class T> class` answers nothing, the defect is in the member query's handling of
a class template's members, which is the fix item 5 wanted in the first place — pointed at the right place.
### Round 9: every idiom the standard library uses, reproduced — all answer correctly

Two more fixtures, both passing:

- `a_member_is_found_through_a_plain_class_and_through_a_template` — the same member through a `struct` and through
  `template <class T> class`, both typed (`int`), so a class template's instantiation is not the gap;
- `a_class_template_in_macro_opened_namespaces_is_the_standard_library_idiom` — the class template inside
  `_STD_BEGIN … _STD_END`, with `_NODISCARD`/`_CONSTEXPR20` on the member and `using reference = value_type&;`, answering
  `int&`. That is MSVC's own idiom for `vector`, reproduced without the standard library.

So the mechanism covers: plain class, class template, dependent alias chain, a header outside the index, and macro-opened
namespaces with macro-guarded members. What remains is specific to the **real** header's text, and the instrument that can
see it is the cooked tree of that file (`inspect tree --file LuaCodeFormat.cpp --cook`, looking for the `std::vector` class
node and its member declarations) — the next round starts there rather than with another simplified fixture.

Item 5 is therefore **not** a missing capability in the design; it is a defect in how one real header is read, and it will
not be claimed fixed until that header's own tree says why.
### Round 10: `<vector>` **is** resolved and reachable — and the instrument to see the rendering does not exist

The include chain was checked end to end:

```text
LuaCodeFormat.cpp  includes  LuaCodeFormat.h  (5 quoted includes, all resolved)
LuaCodeFormat.h    includes  CodeFormatCore/Config/LuaStyle.h   (10 resolved)
LuaStyle.h         includes  <vector>  ->  …/MSVC/…/include/vector     ← resolved
```

So the standard library's `vector` **is** in the include graph of that translation unit, its own header summarizes 993
declarations, and cooking inlines the text. The remaining question is therefore about the **rendering's tree**: does the
`std::vector` class node exist in the cooked tree, and does it carry its member declarations?

**That question cannot be asked today.** `inspect tree` is a *position* query:

```text
$ inspect tree --file LuaCodeFormat.cpp --cook
inspect: give one of `--at`, `--needle`, or `--line`/`--col` to say where the question is
```

and with a position it prints the tree at that node, not the whole tree — so there is no way to look for a class node that
the position does not already contain. This is a gap in the tool, and every remaining question about item 5 needs it.

**The next step is therefore a tool change, not an analysis change**: `inspect tree --whole` (or `--grep <pattern>` over the
rendering's nodes) that prints the nodes whose text matches, with their kind and range. With it, the question is one
command: does the cooked tree of that file contain a `ClassDecl` named `vector`, and what children does it have? Only then
is there a defect in the analysis to name — everything above it has been measured and is right.
### Round 11: `inspect grep` exists — and it shows the cooked tree has **no inlined includes**

`inspect` gained a whole-tree search (commit "inspect: a whole-tree search"), because `tree` is a position query:

```text
$ inspect grep --project EmmyLuaCodeStyle --file CodeFormatLib/src/LuaCodeFormat.cpp --at 0 --cook "class vector"
nodes whose text contains "class vector":
  nothing

$ inspect grep … "emplace_back"
  Syntax(TranslationUnit) 0..10086 #include "LuaCodeFormat.h"
  Syntax(Declaration) 440..885 void LuaCodeFormat::UpdateCodeStyle(…)
  Syntax(Declaration) 747..812 auto &config = _configs.emplace_back(
```

The translation unit is **10 086 bytes — the file's own size**, and the summary of the same file said `cooked reading no`.
So `inspect`'s "cooked" reading for this file is **not** the compiler's translation unit here: the includes are not
inlined, which is the documented artifact ("`inspect` is not the server", `docs/open-questions.md`).

**That invalidates the `0 items` measurement as evidence about the server.** The member query was run against a tree
without `std::vector` in it — so it found no members, exactly as it should. The types fixtures pass because a
`Session` *pumps* its closure (`want_cooked_reading` + `index_everything`); `inspect` does not.

Item 5 therefore has **no reproduction outside the editor** yet, and the next step is a server-path fixture: a `cpp_ls`
handshake or session test over a two-file project where the member comes from an included header, asserting the type
through the same call the server makes. That is the only path that can falsify or confirm the user's report.
### Round 12: two includes away, guarded — correct

`a_member_is_found_two_includes_away_through_guards` passes: `base.h` (guarded) declares the class template, `mid.h`
includes it with `#pragma once`, the `.cpp` includes only `mid.h`, and `auto &result = values.emplace_back();` answers
`int&`. So depth, guards and `#pragma once` are not the gap either.

# Where the three items stand

| item | state | evidence |
|---|---|---|
| 1 — a member position is a field, named from the AST | **implemented** | `NameKind::Property` asked of the binding's *own* scope (binding and index paths); `is_a_constructor_declarator` takes the segments from `qualified_declarator_name`; fixtures `a_mem_initialiser_names_a_field`, `a_constructor_with_a_mem_initializer_is_drawn_as_its_class` |
| 2 — reference attribution by the declaration | **no reproducible defect** | three fixtures: single file, cross file, and a cursor on a *use*; all attribute a member by its declaration, and the other class's member is never listed |
| 5 — a class template's member alias | **no reproducible defect** | six fixtures: chain, dependent alias, header outside the index, plain vs template, `_STD_BEGIN` idiom, two guarded includes; every one answers `int&` |

**What would reopen 2 and 5**: a concrete `file:line` (a symbol and the wrong occurrence for 2; the declaration and the
wrong type for 5) that a session fixture can reproduce. Without it, a change would be a guess, and a guess is what turned
this list into ten rounds of measurements in the first place.

The instrument added along the way stays: `inspect grep` answers "is this node in the tree" for the whole rendering, and
the two failing `inspect` measurements are recorded with why they are artifacts (its cooked reading for a project file
does not inline includes — `docs/open-questions.md`).
## Reproduced through the session, not through `inspect` (round 13)

`crates/cpp_code_analysis/tests/real_project_probe.rs` opens `EmmyLuaCodeStyle` the way the server does — every project
file, the closure pumped — and asks the two questions the editor reports:

```text
[probe] the rendering is 7993 byte(s); `class vector` absent; `emplace_back` present
[probe] std::vector: unknown — NotDeclaredHere("std::vector")
[probe] result: unknown — UnknownType("std::vector<LuaDiagnostic>&")
[probe] the qualifier `LuaCodeFormat` at 8613 is classified Type (declaration: false)
```

Three facts, and the first two are the defect:

1. **The rendering is 7 993 bytes — the file's own text**, with no header inlined. So the cook does **not** inline
   includes; declarations from other files can only come from the index.
2. **The index answers `NotDeclaredHere("std::vector")`** from that file. A system header is read while resolving an
   `#include`, and is **not** part of the index this session built — `add_project_files` names the project's files, and a
   header outside them is never summarized. Measured separately: the MSVC `vector` header *does* summarize 993
   declarations when it is asked for directly, so the reader is fine and the **membership** is what is missing.
3. `result`'s type therefore fails at the first step: `std::vector` is not a type this file can see.

So the fix is **not** in alias substitution, member lookup or the class template: it is that a translation unit's include
closure must be part of what the analysis knows — either summarized into the index as it is read, or inlined into the
rendering. That is the next change, and the probe is the instrument that will say whether it worked (the same three lines
become `class vector present` and `definitions: 1+ declaration(s)`).

The qualifier of the definition is a **Type** in this same run, so the semantic-token report is a stale binary or a
client-side cache: the analysis answers `Type`.