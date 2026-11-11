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