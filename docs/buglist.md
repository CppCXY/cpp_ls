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

## 2. `shared_from_this()` — fixed in `fba6959`

**Symptom (图 6, and again 2026-11-04).** The call had no hover at all; `type_at` answered
`Unknown(Ambiguous("shared_from_this"))`.

**Fixed in `6e6ed31`.** `a_member_of_the_enclosing_class` is C++'s second half of unqualified name lookup — a name inside
a member function is also looked up in the class and its bases — and the hover builds its card from it. Measured:
`shared_from_this()` hovers as `int shared_from_this()` — "`Base::shared_from_this` — a function returning `int`, in
`Base`", declared where the header writes it.

**And the type, in `fba6959`.** `bases_of` answers *names a class can be looked up by* and drops the arguments on
purpose; the walk now carries the **written clause** and runs the base's members through `substituted_members`, so
`Node`'s `shared_from_this` returns `Node *` (`std::shared_ptr<Node>` for the real library) and `declared_in` names the
instantiation. The enclosing class is also asked **before** the index's flat name query — after the file's own scopes —
which is where C++ puts that lookup.

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
| A member inherited from a template base kept the base's parameter (`_Tp`/`T *`) — no `shared_from_this()` type | `fba6959` |
| A braced **construction** in an expression (`auto t = std::tuple{1, 2, 3}`) kept its bare class name | `520cc5c` |
| `typename` in a lambda's template parameter list drawn as nothing, for a client whose legend lacks `modifier` | `c0d932f` |

---

## 9. `typename` in a lambda's template parameter list — fixed in `c0d932f`; the parameter itself is open

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

---

## 11. Inlay hints: what the types cover, and what is left

**Done in `715da0a`.** `auto` variables, a range-for's `auto`, a structured binding's names, and the return type an
`auto` function deduces — `inlay::type_hints`, asking the same reader the hover asks. Measured on `test_cpp` with
`inspect inlay`: `basics.cpp` 9 hints, `smart_pointers.cpp` 8 (`unique_ptr<Resource>`, `shared_ptr<Node>`, …),
`ranges.cpp` 1, `lambdas.cpp` 0.

**Open, in the order they matter:**

* **A lambda parameter's type** — `[](auto x)` is a template; the type exists only at a call site, and this layer holds one
  file. The shape of the fix is clangd's: at the call, the argument's type is the parameter's.
* **A lambda's deduced return** — its type is unnamed and `deduced_returns_of` starts from a *binding*, which a lambda does
  not have.
* **A braced initialisation in an expression** — `auto t = std::tuple{1, 2, 3}` answers `std::tuple`, without its
  arguments: the class-expression path resolves a bare name (`Pair{…}`) but not a qualified one (`std::pair{…}`).
  Measured with `inspect inlay --project … --file src/basics.cpp` (`107:15  : std::tuple`).
* **A structured binding over a user class** — the elements are split for `pair`/`map` by name; a class with `first`/`second`
  of its own answers the aggregate and stops, so `auto [a, b] = my_pair;` gets no hints. (The aggregate is known — CTAD
  answers `Pair<int, double>` — so this is the split, not the deduction.)

---

## 12. A lambda's own template parameter is not classified

Found while testing `c0d932f`: for `constexpr auto kTyped = []<typename T>(T value) { … }`, the encoded tokens carry
`typename` (a `modifier`, or `keyword` when the legend cannot draw a modifier) and **nothing at all** for `T` — while the
report's inspector showed `T` as a `typeParameter`.

`classified_names` classifies a declaration's own name from the scope tree, and a lambda's explicit template parameter
list is not a `TemplateParameters` scope there (a `template <class T>` head is). The fix is in the scope walker
(`sema::scopes`): the list a lambda writes after `[]` opens the same kind of scope, with the same parameter bindings.

**Check.** `inspect` has no semantic-token command yet; the test
`typename_in_a_lambdas_template_parameter_list_is_drawn` holds the fixture and asserts the `typename` half.

---

## 13. `#12` and `#11(d)` — fixed in `19e32a0`

* **A lambda's own `T`.** `[]<typename T>(T value)` bound nothing for `T`: the lambda's scope walker read the parameter
  list and the body, and the explicit `TemplateParameterList` a C++20 lambda may write was never visited. It now opens a
  `TemplateParameters` scope as a `template <…>` head does, and `T` is a `typeParameter`.
* **A structured binding over a project class.** `element_of` is a table of the standard library's shapes; a class with
  `first`/`second` is now asked through `members_of`, with the container's arguments substituted — `auto [a, b] = p;` over
  `Pair<int, double>` binds `int`/`double`.

## 14. The first checkpoint of the card work — and what it uncovered

**Fixed in `13c665f`.** `B<int>{}` was not typed at all while `B{}` was: the name gate (`writes_a_name`) rejects `(`, `)`
and `,` and not `{`/`}`, so a construction with a template-id head was claimed by the *name* arm. And a construction is now
distinguished from a bare braced list by its second child. `Builder<int>{}.with(2)` hovers as `int with(U input)`.

**Open, in the order they matter:**

* **The call's type on a braced construction.** `auto y = B<int>{}.with(2);` answers the initialiser's spelling
  (`B<int>{}.with`) while the hover on `.with` answers `int with(int x)`. The hover reads the member, the call's type does
  not — the callee is an `IndexExpr`-shaped member call, which `declaration_of_a_callee` does not recognise.
* **The template head in the card** (#1's checkpoint): `T with(U input)` still does not say `template <typename U>`. The
  fact carries only the **names** (`["U"]`), not the written header, so composing `template <typename U>` would be a guess
  for a non-type parameter (`template <int N>`). The fix is a fact field holding the written header at index time — the
  same shape as `parameter_list`, which is already the written text.

---

## 15. The five reports of 2026-11-04, and where each one stands

Fixed in `62f0d0c`:

| fig | what | how |
| --- | --- | --- |
| 1 | `__LINE__`, `__FILE__`, `__DATE__`, `__TIME__`, `__cplusplus` drew as "Other" | classified as `macro` from the standard's table and the toolchain's; completed too |
| 2 | the macro card showed the `#define` and left the expansion to the reader | `Expands to` — `Session::the_expansion_of_a_macro`, textual, and the card says which rules it applied |
| 3 | `typename` had a `modifier` colour the client could not map near `static_cast` | the custom semantic type `typename` (fallbacks `modifier`, `keyword`); the VS Code extension maps it to `keyword.operator.cast` |
| 5 | `constexpr auto make_sized()` hovered as "a function" with no return type | `return T{…}` states its type; the card asks the index for a **function**'s facts when the binding's are empty |
| 6 | `make_sized<3>().size()` — "the type is not known here" | a template-id callee is resolved by the name before its `<`, through the scope tree first, so `Size → 3` reaches the return type |

**The extension change is configuration only** (`semanticTokenTypes` + `semanticTokenScopes` in
`C:\Users\zc\Desktop\github\VSCode-Cppls\package.json`); the extension is the reader's to build.

## 16. Fig 7 — what is left of it

Measured with `inspect`: `std::runtime_error` **resolves** — `hover` on the name answers ``class runtime_error : public
exception``, "a type derived from `exception`, in `std`", declared in `stdexcept:221:9`. So the report is not about the class
name. Two things beside it are missing:

* **its base**: `members_of("std::runtime_error")` reports `unlisted base exception: NotDeclaredHere("exception")` — the base
  clause's spelling is looked up as written, and `exception` is `std::exception`. The fix is the base walk's qualification:
  a base found in the enclosing scopes the way a member is (`resolved_in_the_enclosing_scopes` already does this for a
  *member*, and the base walk calls it — so the gap is that the base is written unqualified in `stdexcept` and the lookup
  happens in the *derived* class's file).
* **an inherited constructor**: `using std::runtime_error::runtime_error;` has no reader — the using-declaration binds a
  name, and a constructor is not a name completion can offer on the class. The shape to handle is C++'s
  `using Base::Base`, which makes each of the base's constructors a constructor of the derived class.

## 17. Still open from the earlier rounds

* #1 — the card's **template head**: `T with(U input)` does not say `template <typename U>`, and the fact holds only the
  parameter **names**. The fix is a fact field for the written header, as `parameter_list` already is for the parameters.
* #11(a),(b) — a lambda parameter's type at a call site, and a lambda's deduced return.
* #14 — the **call's type on a braced construction** (`auto y = B<int>{}.with(2)` answers the initialiser's spelling while
  the hover on `.with` is right).
* #12 — done in `19e32a0`.

---

## 18. The declaration vocabulary — `3fc280a`

`typename` was the example, and the request was the whole family: `constexpr`, `auto`, `decltype`, `namespace`, `explicit`,
`noexcept`, `const`, `static`, `class`, `struct`, `union`, `enum`, `template`, `typedef`, `using`, `alignas`, `alignof`,
`consteval`, `constinit`, `volatile`, `mutable`, `extern`, `register`, `thread_local`, `inline`, `virtual`, `friend` — one
colour; `return`, `if`, `while`, `for`, `switch`, `break`, `continue`, `goto`, `case`, `default`, `sizeof`, `new`, `delete`,
`throw`, `try`, `catch`, `co_*` and the `_cast`s — the standard keyword colour. The custom type is `declarationKeyword`
(it was `typename`), the extension maps it to `keyword.operator.cast`, and the fallbacks are `modifier` then `keyword`.

## 19. Ranges — what `4c519d1` and `6d62857` answer, and what they do not

**Answered** (measured, `tests/ranges.rs`): the element of a pipe chain (`numbers | views::filter(f) | views::take(2)` →
`int`), of `views::iota`, of `views::keys`/`values`, of `views::join`; `std::ranges::find`/`max`/`min`/`*_element` → the
element, `count_if`/`distance` → `std::ptrdiff_t`, `size` → `std::size_t`, `subrange` → the element of the range its
iterator came from; `std::views::` completes its adaptors; a loop variable over a chain completes the element's members.

**Not answered, in the order they matter:**

* **`views::transform`'s element** — it is the lambda body's return type, and the deduction refuses rather than guesses.
  The shape of the fix: type the lambda's `return` the way a function body's is typed (`type_a_return_states`), then
  substitute the parameter from the range's element. `tests/ranges.rs` pins the refusal so the change is deliberate.
* **`std::views`' members in the index** — `members_of("std::views")` answers 0 while `std::ranges` answers 125, so the
  *hover* on `std::views::filter` has nothing to say even though completion now offers the name. That is the summary of
  `<ranges>` (or of the `bits/ranges_*.h` that open the namespace) not recording `inline constexpr` adaptor objects —
  worth a probe of one of those files' facts.
* **`ranges.cpp`'s functions** — `pipeline()` and `counted_range()` return `auto` with no written return type, and their
  `return result;` names a **local**, which the syntax deduction refuses (`a_data_member_type` covers members, not locals).
  So `for (int value : pipeline())` has no element. The fix is the same one the deduced-return work opened: type a `return`
  that names a local by the local's declared type.

---

## 20. The three reports of this round — `c354c60`

| fig | what | how |
| --- | --- | --- |
| 1 | `int* a; a[0]` had no type | the subscript arm learned `Type::Pointer` — `p[i]` is `*(p + i)`, whose type is the pointee |
| 2 | `nullptr` drawn as a variable | it is `declarationKeyword` now, the colour `const` has |
| 3 | `std::is_same_v` hovered as two identical lines and "decided by the arguments" | one line per **distinct** signature, and a list with one shape says "a redeclaration, or the two branches of a conditional header" |

And two items from #19:

* **`return` a local** — `auto pipeline() { std::vector<int> result; … return result; }` now answers `std::vector<int>`. The
  small walker (`deduced_returns_of`'s own) learned a member and a local; the local's type is the last declaration before the
  return that declares the name.
* **A namespace alias's members** — `members_of("std::views")` answered 0 and now answers 100: libstdc++ writes
  `namespace views = ranges::views;`, and the member walk follows *type* aliases only, so an empty answer for a namespace
  whose fact names a target is asked again of the target.

**Still open from #19:**

* **`std::views::filter` has no hover.** The name query (`definitions_in_scope`) resolves the scope `std::views` without
  following the namespace alias, so `define`/`hover` say `NotDeclaredHere("std::views::filter")`. The member list follows the
  alias, completion offers the name from its table; the fix is the same following in the **scope lookup of a qualified
  name**.
* **`views::transform`'s element** — the lambda body's return type is still refused.
