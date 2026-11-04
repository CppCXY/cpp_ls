# The bug list — open, measured, ordered

What is left after the template/member work of `7e44571` … `5891719`, each item as it was reported, what was measured
about it, where the cause is, and how to check a fix. The rule this list keeps: **no item without a measurement**, and no
item kept alive by "it might be a problem" — every one of these was seen in the editor on `C:\Users\zc\Desktop\test_cpp`
or in a probe, and a few of them are explained and kept because the fix is a design decision rather than a patch.

Every command below is `tools/inspect` (`cargo run -q --release -p inspect -- …`); `$p` is that project.

---

## 1. The card is composed text, not a signature — *the one that matters*

**Symptom (图 7).** Hovering a declaration shows the declaration's raw text plus one sentence:

```text
```cpp
std::shared_ptr<Node> twin()
```

`Node::twin` — a function returning `std::shared_ptr<Node>`, in `Node`

Declared in `smart_pointers.cpp:27:27`
```

That is right for `twin`, and it is not a signature: a template's head (`template <typename Fn>`), a parameter's **type**
(when the parameter list is only names), a default argument, an unnamed parameter, a `constexpr`/`static` prefix and the
requires-clause of an *overload set* are all facts the index records and the card does not compose.

**Measured.** `inspect facts --file src/templates.cpp --name with` prints `parameters` as the fact holds them; the card
prints `parameter_list` only. `Builder::with` is a member template whose parameter is written `U input` — the card shows
`T with(U input)` but not that `U` is a template parameter nor what it defaults to.

**Where.** `crates/cpp_ls/src/handlers/hover/mod.rs` — `signature_line` (returns + name + `parameter_list` + trailing
clause) and `declaration_markdown` (the raw block for everything else).

**Fix.** Compose from the fact in a fixed order: template head (`fact.parameters`, with each parameter's constraint and
default) → specifiers (`modifiers`: `static`, `constexpr`, `explicit`, …) → return type → qualified name → parameter
list, with each parameter's **type** when the declaration wrote one and a name when it did not → qualifiers (`const`,
`&`, `&&`, `volatile`) → `requires_clause`. Then keep the raw declaration only for what a fact cannot express (a class
body, a `using`, an operator's spelling).

**Check.** `inspect hover --project $p --file src/strings.cpp --needle "cfoo.call" --shift 5` must answer
`void call(Fn &&fn) const requires requires { fn(*this); }` (it does today, from the qualifier work) **and**
`inspect hover --project $p --file src/templates.cpp --needle "Builder<int>{}.with"` must show the template head.

---

## 2. `shared_from_this()` — the hover is fixed, the *type* is still open

**Symptom (图 6, and again 2026-11-04).** The call had no hover at all; `type_at` answered
`Unknown(Ambiguous("shared_from_this"))`.

**Fixed in `6e6ed31`.** `a_member_of_the_enclosing_class` is C++'s second half of unqualified name lookup — a name inside
a member function is also looked up in the class and its bases — and the hover builds its card from it. Measured:
`shared_from_this()` hovers as `int shared_from_this()` — "`Base::shared_from_this` — a function returning `int`, in
`Base`", declared where the header writes it.

**What is still open: the call's *type*.** `members_of("Node")` carries the member with `returns: "_Tp"` — the **base's**
parameter — and the pairing that would finish it (the base's arguments, `Node`) is not on the member list: `declared_in` is
`std::enable_shared_from_this`, without `<Node>`. The type engine refuses an unfinished type rather than printing
`_Tp (*)()`, so today the answer is `Unknown`. The fix is one field: the base walk knows the base's spelling and its
arguments, and `ProjectMember` should carry them (or `declared_in` should be the instantiated spelling).

**Measured.**

```text
inspect type --project $p --file src/smart_pointers.cpp --needle "shared_from_this()" --shift 2
→ Unknown(Ambiguous("shared_from_this"))
inspect members --project $p --file src/smart_pointers.cpp --class Node
→ 12 members, including `shared_from_this` from `std::enable_shared_from_this<Node>`
```

So the member is **there**, through the base; the *name lookup* is what fails, because `enable_shared_from_this` declares
two overloads (const and non-const) and an unqualified name inside a member function is looked up in the scope tree, which
has no notion of "the enclosing class's members".

**Where.** The name query (`Session::definition`) and the type engine's name arm share one answer; both would need the
same step: **an unqualified name inside a member function is also looked up in the enclosing class** (and its bases).

**Fix.** `class_of_the_enclosing_definition` already exists (it is how `this` is typed). A name the scopes cannot place
asks `members_of(that class)` and answers the fact whose `declared_in` is the class or a base — with the base's arguments
substituted for the return type (`shared_ptr<_Tp>` → `shared_ptr<Node>`). That is C++'s unqualified member lookup,
restricted to the one case the analysis can be sure about.

**Check.** `inspect type … --needle "shared_from_this()"` must answer `std::shared_ptr<Node>`, and the hover must name
`std::enable_shared_from_this<Node>::shared_from_this`.

---

## 3. `std::any any = 1;` hovers as `void any;` — and `std::pair pair{1, 2};` as `void pair;`

**Symptom (today).** The card says `void any;` where the declaration wrote `std::any`, and `void pair;` for `std::pair`.

**Measured.** The **facts are right**:

```text
inspect facts --project $p --file src/containers.cpp --name any
→ "any"  variable  type_of `std::any`  at 97:9
inspect facts --project $p --file src/containers.cpp --name pair
→ "pair" variable  type_of `std::pair` at 104:9
```

So the summary knows the type and the card prints something else. The card's block for a variable is
`signature_line`: `type_of` + name. `void` is what `parse_type_spelling("")` answers, so somewhere an **empty** spelling
is parsed into a type — the facts above come from the summary, while the hover's declaration comes from the **binding**
path (`fact_from_binding_with`), which builds `type_of` from the shape table. The two disagree, and the second one is
empty for a declaration whose type is a **qualified name in another file**.

**Where.** `crates/cpp_code_analysis/src/index/project.rs` — `fact_from_binding_with` /
`declared_type_of_with` for a binding whose declaration is `std::any any = 1;`; and
`crates/cpp_code_analysis/src/sema/types.rs` — `parse_type_spelling("")` answering `void` rather than "no type" (an empty
spelling is not a type; `Unknown` is the honest answer).

**Next step.** Ask the binding path directly (a `facts` query reads the summary; the hover reads the binding). The box to
open first: does `fact_from_binding_with` answer `Some("")` for these two lines, and if so which shape it read it from.

**Check.** `inspect hover --project $p --file src/containers.cpp --needle "std::any any" --shift 9` must show `std::any any`.

---

## 4. A declaration shaped like "the most vexing parse" has no type

**Symptom (图 5).** `std::shared_ptr<int> aliased(shared, nullptr);` — hovering `aliased` shows the type as the **whole
initialiser** and its members cannot be read.

**Measured.**

```text
inspect facts --project $p --file src/smart_pointers.cpp --name aliased
→ "aliased" variable type_of <none>
inspect type --project $p --file src/smart_pointers.cpp --needle "aliased.use_count" --shift 8
→ Unknown(UnknownType("std::shared_ptr<int>(shared, nullptr)"))
```

The declaration parses as a **function** `aliased(shared, nullptr)` returning `std::shared_ptr<int>` — legal C++ for both
readings, and the language decides by whether the arguments name types. `custom` on the next line
(`std::unique_ptr<int, FreeDeleter> custom(new int(3));`) gets its type right, so the parser is not simply treating every
`T x(a, b);` as a declaration.

**Where.** The parser's declaration/declarator decision (`crates/cpp_parser`), and `declared_type_of_with`'s fallback for
a binding of kind `Function` whose name sits in a `T x(args);` shape.

**Fix, and the cheap half of it.** The type question does not need the parser to be right: when a fact is a function with
**no parameters that parse as declarations** and a `parameter_list` that parses as expressions, the declaration is a
variable of the return type. That fallback belongs in `declared_type_of_with`, next to the other shape questions.

**Check.** `inspect type --project $p --file src/smart_pointers.cpp --needle "aliased.use_count"` must answer `long`.

---

## 5. A `const` object still shows the non-const overload first

**Symptom (图 3).** `cfoo.call(...)` on a `const Foo` lists both `call`s; which one is callable is a sentence, not the
answer.

**Measured.** `type_of` carries no qualifiers: `Foo foo;` and `const Foo cfoo;` both answer `Foo`
(`inspect type … --needle "cfoo"`).

**Fix.** A qualifier on the **type**, not on the fact: either `Type::Named` grows `is_const` or `ExpressionType` carries
it beside `type_of`. Then `member_markdown` picks the overload that is callable, and `sema::check` could say when **none**
is (calling a non-const member on a const object is an error a reader would like to see).

---

## 6. A class template's deduction still needs its guide

**Symptom.** `Wrapper{1}.value` is `T`; the guide `Wrapper(int) -> Wrapper<long>;` is parsed and not used.

**Measured.** `inspect type --project $p --file src/templates.cpp --needle "Wrapper{1}.value" --shift 12` → `T`.

**Fix.** At a braced initialisation of a class template with no written arguments, look for a `deduction guide` fact
(`DeclKind::Function`, name = the class's name, no scope) whose parameter list accepts the initialiser, and substitute
its return type's arguments. Deduction guides are already *parsed* as the class's constructors, which is the shape the
lookup would need.

---

## 7. Go to Definition on a `#define` — *needs a reproduction*

**Symptom (图 2).** "Find definition" on a macro use jumps to wrong positions while references are right.

**Measured.** The analysis answers correctly on both macros tested:

```text
inspect macro --project $p --file src/preprocessor.cpp --needle "TEST_CPP_SQUARE" --last
→ TEST_CPP_SQUARE is defined at src/preprocessor.cpp:9:9
inspect macro --project $p --file src/preprocessor.cpp --needle "TEST_CPP_MAX" --last
→ TEST_CPP_MAX is defined at src/preprocessor.cpp:10:9
```

and the member cards' positions were wrong until the ruler fix in `7e44571` (`Declared in templates.cpp:18:7` now). Kept
open until the exact symbol and file of the screenshot are known: if it still reproduces, the next question is whether the
`Location` the handler returns for a macro is the **name's** range (it is) or a client-side `#include` resolution.

---

## 8. The session's own cost: a cold closure is minutes single-threaded

**Symptom (and why `inspect` defaults to lazy).** `inspect --cook --index` on this project takes **212–347 s** on a cold
summary cache; the same question without them takes **90 ms**.

**Measured.**

```text
type, default                        90 ms
type --cook --index, cold cache     212 s
members --cook --index, cold cache  347 s
```

The work is the file's include closure (~1 700 files, the standard library) indexed **on one thread**, and it is the same
work the server does in **18 s** on all cores at startup. This is not a bug in the tool — but it is the reason the tool's
expensive steps are flags, and the reason a `batch`/`session` mode (one cold start, many questions) is worth having.

**Fix.** `inspect batch --questions questions.json` — one session, one cold start, N answers, the way a real session works.
Planned, not built.

---

## What is already fixed, for the record

| report | commit |
|---|---|
| `Pair<int, int>{1, 2}.sum()` → "`int` has no member `sum`" (the object was typed `int`, the member `T`) | `7e44571` |
| A declaration's card position was the rendering's ruler (`templates.cpp:6:12` for line 18) | `7e44571` |
| `const`/`&`/`&&` and the requires-clause missing from the card — and from the fact | `c08e213` |
| `std::views::` answered 0 names after a header's alias stopped being in the buffer's tree | `c08e213` |
| `T(args)` had no type; `->` through a smart pointer looked in the handle; `make_shared`'s wrapper type | `e47996d`, `5891719` |
| `std::any any;` / `std::pair pair;` hovered as `void` (a type dropped as if it were the declarator) | `90f1726` |
| `std::pair pair{1, 2}` kept its bare name — CTAD at a braced initialisation; a member of a class template was called a "variable template" | `6e6ed31` |
| An unqualified name inside a member function was not looked up in the class (`shared_from_this()` had no hover) | `6e6ed31` |
| A function's name answered its **class** as its type (`shared_from_this` in `Base` was a `Base`) | `6e6ed31` |
| `members_of("shared_ptr<int>")` answered nothing while the completion at the same cursor offered `reset` | `6e6ed31` |

---

## 9. `typename` has no semantic token in a lambda's template parameter list

**Symptom (图 3).** `[]<typename T>(T value)` — the inspector shows `<typename·T>` with **no** token for `typename`
(standard token type `Other`), while `T` is a `typeParameter`.

**Why it is there.** `semantic.rs` classifies **identifiers only** (`token.kind != CppTokenKind::Identifier { continue }`),
and its module documentation says so on purpose: "**no** token for `int`, `return`, or a string literal: a client that
already lexes C++ draws keywords itself."

**Why it still matters.** A client's grammar does not always recognise `typename` in that position, and the one place it
matters to this server is the *template parameter list* — where a reader needs to see that `T` is a parameter of a
template. Two options, both defensible:

* a narrow exception: `typename`/`class` **introducing a template parameter** get a `keyword` token (a new `NameKind`,
  mapped in the legend), with the module's rule kept for every other keyword;
* nothing, and the client's grammar is where the fix belongs.

**Check.** `inspect tree` locates the token; the classification is `Session::classified_names`.

## 10. Go to Definition on `make_incrementer` — *verified correct, not a bug*

**Symptom (图 4).** "跳转到定义的位置完全是错的", with `lambdas.cpp:75:11` in the screenshot.

**Measured.** `inspect define --project … --file src/lambdas.cpp --needle "instance.make_incrementer()" --shift 11`
answers `lambdas.cpp:20:10   make_incrementer   function in Counter parameters ()` — the declaration, which is where the
jump should go. `75:11` is the **card's** "Declared in" line for `increment`, the structured binding, which is correct for
that name. Kept here so the next report of it starts from a measurement: if a client still jumps to 75:11 on
`make_incrementer`, the question is which position it sent.

The card in that screenshot is `increment` — a structured binding over `std::pair{by_reference, by_value}`, whose element
types are **lambdas** and therefore unnamed. Its sentence ("the type is deduced from the initializer, and that could not be
typed: …") is the analysis's honest refusal, and it is long enough that a narrow popup wraps it mid-clause; the wording is
worth shortening, the answer is not wrong.
