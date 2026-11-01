# C++ support

What this server understands about C++: how a file is identified as C++, which language features it reads, what the
semantic layer can answer, what each LSP feature gives, and which diagnostics exist. Configuration and the limits are in
[`configuration.md`](configuration.md); the same reference for C is [`c-support.md`](c-support.md).

## 1. Language identification

| Item | Rule |
| --- | --- |
| Extension | `cpp`, `cc`, `cxx`, `c++`, `cppm`, `ixx` are sources; `h`, `hh`, `hpp`, `hxx`, `h++`, `ipp`, `inl`, `tpp`, `tcc` are headers |
| `.h` | belongs to whichever language **includes** it; read on its own it is C++, because that search list is the wider one |
| `-std=` | wins over the extension: `c++11`…`c++23` and `gnu++…` are C++; `c…` is C |
| Range of standards | **C++11 and later** (there is no separate C++98/03 mode; older code is read as C++11 and usually still parses) |
| Indexed extensions | `c cc cpp cxx c++ h hh hpp hxx h++ ipp inl tpp tcc cppm ixx`, plus anything in `[workspace].source_extensions` (`cuh`, `cppm`) |
| Where files come from | `compile_commands.json` → `.cppls.toml` → the project scan; an `#include` no search path can place falls back to matching the tail of its path against the tree |
| CUDA | `.cu`/`.cuh` files and an `nvcc` build are read as CUDA; `[compile.cuda]` turns that on without a database |

## 2. Standards and language features

Each construct is a **feature bit** opened by `-std=` or `[compile].standard`.

| Standard | Constructs accepted |
| --- | --- |
| C++11 | raw strings `R"(…)"`, `auto`, lambdas, rvalue references `&&`, variadic templates, alias templates `using`, `[[attribute]]`, `static_assert`, `alignas`/`alignof`, `thread_local`, `nullptr`, `enum class`, range `for` |
| C++14 | generic lambdas (`auto` parameters), variable templates, binary literals, digit separators `'`, return type deduction |
| C++17 | structured bindings, `if constexpr`, fold expressions, `inline` variables, class template argument deduction (CTAD), nested namespaces `a::b::c`, `[[maybe_unused]]`/`[[nodiscard]]`/`[[fallthrough]]` |
| C++20 | concepts and `requires`, modules (`module`/`import`/`export`), coroutines (`co_await`/`co_yield`/`co_return`, `operator co_await`), designated initialisers, the three-way comparison `<=>`, `using enum`, `char8_t`, `consteval`, `constinit` |
| C++23 | deducing `this` (explicit object parameters), `if consteval`, multidimensional subscript `a[i, j]`, static `operator()`/`operator[]`, `#elifdef`/`#elifndef` |
| GNU extensions | statement expressions, `__typeof__`, `__attribute__`, `__builtin_*`, `asm`, `__restrict__` |
| MSVC extensions | `__declspec(...)`, `__int64`, `__try`/`__except`/`__finally`, `__cdecl`/`__stdcall`, macro prefixes such as `_STD_BEGIN`/`_STD_END` |
| CUDA | `__global__`/`__device__`/`__host__`, the `<<<…>>>` launch syntax, built-in vector types such as `float4` and `dim3` |

**Not supported**: C++26 (contracts, reflection, `#embed`) has no feature bits yet; that syntax is read as C++23.

## 3. Preprocessing

| Ability | What it does |
| --- | --- |
| `#include` | angle and quote forms; search paths from the database, the configuration or the toolchain; a header is resolved against the C++ list, with tail matching as the fallback |
| `#define` / `#undef` | object-like and function-like, variadic, `#`, `##`, `__VA_OPT__`; `#undef` is part of the history |
| Conditionals | `#if`/`#elif`/`#else`/`#endif`, `#ifdef`/`#ifndef`, C++23's `#elifdef`/`#elifndef`; expressions are evaluated as integer constants |
| Modules | `module`, `import`, `export` and `export module` are parsed and indexed as declarations |
| Predefined macros | `__cplusplus` follows the selected standard; the toolchain's markers (`_MSC_VER`, `_WIN32`, `__GNUC__`, `__clang__`) follow the compiler |
| Macro environment | the set of macros visible at each position is recorded, and hover, completion, references and rename ask it rather than searching the text |

## 4. Semantic analysis

This is the "understands C++" layer, and the base every completion, hover and check stands on.

| Ability | What it answers |
| --- | --- |
| Scopes | translation unit, namespaces (anonymous, inline, and **aliases** — `namespace views = ranges::views;` resolves to the scope it names), classes/structs/unions/enums (with anonymous-member promotion), functions, blocks, lambdas, `try`/`catch`, `for` init-statements, structured bindings |
| Types | built-in types, pointers/references (lvalue and rvalue)/arrays/function pointers, `const`/`volatile`, `typedef`/alias templates, class template specialisations, `decltype`, `auto`, `char8_t` |
| Template deduction | function template argument deduction (`T`, `const T&`, `T*`, `T[]`, a one-level template-id), explicit template arguments, parameter packs, partial ordering (the more specialised wins), substitution into a class template's members |
| Overload resolution | viable candidates, then a per-argument rank (Exact / Promotion / Conversion / Unranked); the template-versus-ordinary tie-break; a `= delete`d candidate is never selected; several declarations of one name are chosen between by the arguments |
| Type inference | `auto` (including `auto&&`/`auto*`), `decltype`, function return types, the return type of factories such as `std::make_unique` |
| Members | class member lists (inherited, access-controlled, `static`, `using` declarations); a class template's members substituted with the arguments; the `->` of `unique_ptr`/`shared_ptr` and kin follows `element_type` |
| Calls | argument count and types, variadic functions, default arguments as viable (a parameter list may be longer than the argument list), return type, the object type of a member call, `operator()` |
| Concepts | `requires` expressions, `concept` definitions and constraint checking (`concept-not-satisfied`) |
| Coroutines | `co_await`/`co_yield`/`co_return`, `promise_type`, `operator co_await`, and a coroutine's return type entering `auto` deduction |
| Constants | `#if`, array extents, enumerator values, `constexpr` conditions (which branch of `if constexpr` is taken) |
| The index | the cross-file declaration table, the `#include` graph, member-list caches, the summary cache (`.cppls`); modifiers (`virtual`, `override`, `final`, `constexpr`, `static`, `maybe_unused`, `= delete`, …) become **bits on the fact at index time** |

## 5. LSP features

| Feature | What it gives |
| --- | --- |
| `textDocument/completion` | names in scope (locals → class members → namespaces → included headers), the right of `.`/`->`/`::` by type (a **namespace alias** offers the names of its target, whether the alias is written here or in a header), macros, keywords, `#include` file names; with the range to replace, a `sortText` that is the analysis's own order (a local before a keyword before a name from a header), snippets when the client supports them, and `completionItem/resolve` for the one item being looked at |
| `textDocument/hover` | macros first, then the declaration (signature/type/scope), then the type of the expression; the comment next to the declaration comes with it |
| `textDocument/signatureHelp` | the list of overloads with `activeParameter` — the position of the argument being typed |
| `textDocument/definition` | the declaration of a macro, type, function, variable, member or enumerator, across files through the index |
| `textDocument/implementation` | a virtual function's declaration → its overriders; a function declaration → its definition |
| `textDocument/typeDefinition` | a variable or `auto` name → the definition of its type (classes, alias templates) |
| `textDocument/references` | **exact references for macros**, including the ones reached under a conditional `#include` or `#if` (marked as uncertain); an ordinary name is not answered |
| `textDocument/rename` + `prepareRename` | **macros only**; an ordinary name is refused, because editing half of a name's uses is a broken build written into the user's files |
| `textDocument/documentHighlight` | occurrences of the same name in the current file |
| `textDocument/documentSymbol` | the file's own declaration tree (including what `#if 0` removes), namespaces and classes nested; a range for the declaration and one for the name |
| `workspace/symbol` | the declarations the project index holds, by name |
| `callHierarchy` | incoming and outgoing calls of a function (member functions by the object's type) |
| `textDocument/selectionRange` | the chain of syntax nodes around the cursor |
| `textDocument/foldingRange` | brace blocks, comments, `#region`, runs of `#include` |
| `textDocument/documentLink` | the path of an `#include`, as a link to the file |
| `textDocument/inlayHint` | parameter names at call sites |
| `textDocument/codeAction` | `source.include`: an unresolved name → an edit adding the header that declares it, chosen by the include-visibility rule |
| `textDocument/semanticTokens` | the standard types plus `decorator` (Doxygen commands) and the custom `variableTemplate`; inside string literals, `std::format`/`fmt`'s `{}` fields and the `printf` family (`formatSpecifier`, `formatArgument`, `formatConversion`, `formatEscape`) |
| `textDocument/diagnostic` | **pull mode**: syntax errors plus the rules in §6 |
| `textDocument/formatting`, `rangeFormatting`, `willSaveWaitUntil` | whole-file and range formatting, reading the common subset of `.clang-format` (indent, column limit, brace style, short blocks on one line, …) |
| `workspace/didChangeConfiguration` | `.cppls.toml` and editor settings — see [`configuration.md`](configuration.md) |

## 6. Diagnostics

Thirty-one rules, each with a fixed severity; `[diagnostics].disable` turns one off by name.

| Code | Severity | Meaning |
| --- | --- | --- |
| `syntax-error` | Error | the text does not parse |
| `param-type-check` | Error | an argument's type cannot go to the parameter |
| `assign-type-check` | Error | an assignment or initialisation does not fit the type |
| `undefined-field` | Error | the member is not in the type (a class template's member list is a pattern and is skipped) |
| `undefined-name` | Error | the name is not declared |
| `redefinition` | Error | the same entity is defined twice in one scope |
| `argument-count` | Error | the call passes the wrong number of arguments |
| `return-type-mismatch` | Error | a `return` does not fit the declared type |
| `dangling-return` | Error | a reference, pointer or view of something that dies when the function returns — a local, a temporary, or a pointer a local handed out |
| `dangling-field` | Error | a view, reference or pointer member initialised from a by-value parameter or a temporary, which do not outlive the object |
| `use-after-free` | Error | a pointer read after the memory it refers to was released — by `delete`/`free`/`destroy_at`/`reset()`, or by a call to a function whose fact says it frees that argument |
| `ue-naming` | Hint | Unreal Engine's prefix conventions — `UCLASS` → `U`/`A`, `USTRUCT` → `F`, `UENUM` → `E`, a boolean member → `b…` — **UE** (`[unreal] enable`) |
| `ue-generate-body` | Error | a `UCLASS`/`USTRUCT` whose body writes no `GENERATED_BODY()` — **UE** (`[unreal] enable`) |
| `include-not-found` | Error | the `#include` was not found |
| `access-invisible` | Error | a member reached from somewhere that may not reach it |
| `missing-return` | Error | a function that promises a value can reach its end |
| `concept-not-satisfied` | Error | a constraint is not met |
| `macro-redefined` | Warning | the same macro is redefined with a different body |
| `unnecessary-branch` | Warning | a branch that cannot run (`if constexpr`/`if consteval` are not reported) |
| `assignment-in-condition` | Warning | an assignment inside a condition |
| `c-style-cast` | Warning | a C-style cast (a `(void)` cast is not reported) |
| `macro-parentheses` | Warning | a macro body without parentheses of its own |
| `unused` | Hint | a name nothing reads (tag: Unnecessary; `[[maybe_unused]]` and RAII guards are not reported) |
| `braces-around-statements` | Hint | a single-statement branch without braces |
| `use-nullptr` | Hint | `0`/`NULL` used as a null pointer |
| `use-override` | Hint | an overriding function without `override`/`final` |
| `simplify-boolean-expr` | Hint | a boolean expression that can be simplified |
| `else-after-return` | Hint | an `else` after a `return` (`if constexpr`/`if consteval` are not reported) |
| `use-auto` | Hint | a declaration that could be written `auto` |
| `implicit-bool-conversion` | Hint | an arithmetic value converted to `bool` |
| `module-note` | Information | a note about a module's position (not a complaint, no squiggle) |

## Where C++ templates stand, measured

A `techniques.h` + `techniques.cpp` pair in `C:\Users\zc\Desktop\cpp_project` covers the classic shapes: `enable_if` on
the return type, the `void_t` detection idiom, a `decltype` trailing return, a variadic template with a fold expression, a
partial specialisation, a template template parameter, a C++20 concept with `requires`, CRTP, an alias template, and a
class template with a deduction guide.

**All of it parses**: three files, 0 parse errors, 68 declarations, indexed in 2.7 s.

```text
  FAIL  src/techniques.cpp  box.get()     0 offered   `get` is not reachable
  FAIL  src/techniques.cpp  box.set(9)    0 offered   `set` is not reachable
  FAIL  src/techniques.cpp  first.self()  2 offered   `self` missing from ["identifier", "size"]
  FAIL  src/techniques.cpp  box           → techniques::Box      (a deducing `Box box(7)`: no `<int>`)
```

* the first two are **`include-not-found`**, not templates: `#include "techniques.h"` from `src/techniques.cpp` did not
  resolve, so the header was never read and `Box`'s body is unknown. The same class template in one file answers `get`
  correctly, which is what makes this a resolution question. `include_roots_above` starts at the *including file* and
  joins `include` onto it, so its first candidates are `src/techniques.cpp/include` and `src/include` — where to look next.
* the third is a real template gap: `Widget : techniques::counted<Widget>` needs the **CRTP base's** members, and only
  `Widget`'s own two are offered.
* the fourth is CTAD: `Box box(7);` types as `techniques::Box` (no argument) while `Box<int> box(7)` is right. The
  deduction guide parses; it is not applied.

## Update: `include/` beside `src/`, and what that fixed

`include_roots_above` tested `files.exists(<dir>/include)` — a question about a **directory**, which a file provider
answers "no" to, so the climb found `<root>/include` and threw it away. A quoted include from a project laid out
`include/` + `src/` (CMake's own convention, with no compilation database) then reported `include-not-found`, the header
was never read, and every member question about a type it declares answered nothing. It now asks whether the include would
be *found* there: `files.exists(<dir>/include/<name>)`.

Measured on `cpp_project` with the techniques pair added:

```text
                        before                    after
include-not-found       1 finding                 0 findings
members                 1/5                       5/5
box.get() / box.set()   0 offered                 2 offered
first.self()  (CRTP)    `self` missing            5 offered
```

**Left: CTAD.** `Box box(7)` types as `techniques::Box` — the template's name without the deduced argument — while
`Box<int> box(7)` is right. The deduction guide parses; it is not applied.
## Update: CTAD — `Box box(7)` is `Box<int>`, including through a header

Three defects, each narrowed by a failing test before it was fixed:

1. **The initializer was looked for one level too high.** A binding ranges over the `InitDeclarator` that wraps the
   declarator — `Box box(7);` is `InitDeclarator > Declarator > [NameExpr, Initializer]` — so the walk answered "this
   declaration has no initializer". It descends one level, and unwraps a paren initializer (`ArgumentList`, not an
   expression).
2. **The constructor was looked for in the class's scope**, where a constructor is not a binding (a scope is keyed by
   name; `Box::Box` is not a name anything looks up). It is read from the class's own body by matching `[explicit] Name (`
   — and *not* from the specifier sequence, which holds `explicit` alone when the constructor has a member-initializer
   list.
3. **The class's body is not in the file that uses it.** A `.cpp` that includes the header gets the header's facts inlined
   and not its body, and the index keeps no fact for a constructor. The declaring file is parsed through the request
   layer's file provider and the same scan runs over its tree.

```text
                before                 after
box.get()       —                      2 offered            ✓
first.self()    `self` missing         5 offered            ✓  (CRTP base)
Box box(7)      techniques::Box        techniques::Box<int> ✓
```

`cpp_project`: **0 parse errors, 0 findings, 5/5 members, 4/4 types**. Reading without a diagnostic, in the same three
files: `enable_if` on the return type, the `void_t` detection idiom, a `decltype` trailing return, a fold expression, a
partial specialisation, a template template parameter, a C++20 concept with `requires`, CRTP, an alias template, and a
deduction guide.

**Not done, and the honest next step:** the deduction covers *one* template parameter whose constructor takes exactly that
parameter. Two parameters, a constructor taking `Container<T>`, or a hand-written guide (parsed, not read) all answer the
name as written — and `enable_if`'s condition is read as a type rather than **evaluated**, so `describe(41)` resolves but
nothing decides which overload the condition selects.

## Update: a hand-written deduction guide is read, not just parsed

```cpp
template <typename T> struct Wrap { Wrap(std::string); T get() const; };
template <typename T> Wrap(T) -> Wrap<T>;

Wrap wrapped(7);      // Wrap<int> — and nothing about Wrap's constructor says so
```

The guide was grammar the reader accepted and nothing consulted, so `Wrap wrapped(7)` answered the bare `Wrap`. It is
read now: the declaration whose text holds `->`, whose head is `[template <…>] Name(params)` with the class's own name,
and whose tail is exactly `Name<T>` for the class's single template parameter — asked **before** the constructor
heuristic, because when a guide exists it *is* the rule the language applies.

Only the identity form (`Name<T>`). `Name<T*>`, two parameters and a pack are pattern matches this layer does not do, and
a wrong argument prints an instantiation nobody asked for.

## `enable_if`: the evaluator is written, the overload set is not

The condition of an `enable_if` overload is now decidable (`sema::constraint`, pinned by
`the_constraint_evaluator_decides_the_fundamental_traits`):

```text
is_integral<int>            Some(true)     is_integral<std::string>   Some(false)
is_integral<U>              None           is_user_defined<int>       None
!is_integral<double>        Some(true)     is_integral<T> && !is_same<T, char>
```

It reads `::value`, `{}`, `_v` and `()` spellings, composes `!`/`&&`/`||` at the top level, and knows `is_same`. `None` —
*cannot tell* — is answered for an unbound parameter and for a trait it does not know, because deleting a candidate on
"cannot tell" is how a reader loses the overload their program chose.

**What blocks the consumer, measured:** two declarations of one name in one scope produce **one** binding in the scope tree.
A file with the two `describe` overloads binds a single `describe`, so `definitions_at`'s plural form can only return one,
and `go-to-definition` on `describe(1.5)` answers the *integral* declaration because it is first. Narrowing needs a set,
and there is no set to narrow — the next step is structural: an **overload set per name** in the scope tree, after which the
narrowing is a dozen lines against `definitions_at`.

## `enable_if` overloads: chosen by their condition

The scope tree was never the blocker. Measured: the translation unit holds `describe` **twice** — offsets 85 and 208, both
`Function` — so an overload set is represented, and `definitions_at` (the plural query written *for* calls) returns both.
What answered was the **singular** query, which takes the first binding of the name and stops.

Three things then had to be right, each found by a trace rather than guessed:

1. **the arguments are the call's children after the callee**, not always wrapped in an `ArgumentList`:
   `describe(1.5)` is `CallExpr > [IdentifierExpr, LiteralExpr]`;
2. **the declaration, not the declarator**: a binding's range is `describe(T value)`, and the `enable_if` is a specifier
   of the enclosing `Declaration`;
3. **the condition evaluator** (`sema::constraint`) binds `T` to the argument types in order and answers
   `Some(true)`/`Some(false)`/`None` — `None` keeps the candidate, because deleting on "cannot tell" loses the overload the
   program chose.

```text
describe(1.5)  → std::is_floating_point<T>   (T = double)
describe(41)   → std::is_integral<T>         (T = int)
```

Both directions are asserted (`an_enable_if_overload_is_chosen_by_its_condition`): a narrowing that answered the *last*
candidate for every call would pass the first assertion and be just as wrong.

**Still not done:** `enable_if` in a *template parameter* (`template <typename T, typename = enable_if_t<…>>`), a
hand-written `requires` clause, and any trait outside `is_integral`/`is_floating_point`/`is_arithmetic`/`is_pointer`/
`is_void`/`is_signed`/`is_unsigned`/`is_same` — each answers `None`, which keeps both candidates rather than guessing.

## A named concept decides the overload too

```cpp
template <typename T> concept integral = std::is_integral_v<T>;
template <integral T> std::string describe(T value);
```

The third spelling, and the one with no expression in the declaration at all: `enable_if` is not in the text and there is no
`requires` clause, so both overloads stayed candidates and the call answered the first (measured: `describe(1.5)` resolved to
the integral declaration, offset 168 rather than 243).

`a_concept_name` reads the name from the two places it can stand — `template <integral T>` (a parameter whose first word is
a name and whose second is the parameter it constrains, which excludes `typename T`, `class U` and `auto`) and
`requires integral<T>` — and `the_concept_definition` finds the concept's right-hand side in the tree, evaluated with the
same bindings the other forms use.

**Only the definition is expanded.** `concept signed_integral = integral<T> && is_signed_v<T>` names another concept that
this layer cannot expand, so the verdict is `None` and both candidates stay — the safe half of "cannot tell".

All four spellings now resolve both directions:

```text
an_enable_if_overload_is_chosen_by_its_condition   the return type
an_enable_if_in_a_template_parameter_is_read      the template parameter
a_requires_clause_is_read                         C++20's own clause
a_named_concept_decides_the_overload              a concept the file defines
```

## A composed concept decides the overload — and the substitution bug behind it

`concept signed_integral = integral<T> && is_signed_v<T>` was the shape answered "cannot tell" on purpose last round. It
decides now, and the reason it could not before was **not** the concepts:

**`substitute` destroyed the punctuation.** It split a condition into words, joined them with a `\0` separator, replaced the
parameter and joined them again *without restoring the separators* — so `std::is_integral_v<T>` became
`stdis_integral_vdouble`. Every condition naming a qualified trait was unreadable after substitution; the earlier tests
passed only because their arguments were bare (`T → double`).

`satisfies_with` now decides a condition and, when a trait name is one it does not know, asks the caller for that
**concept's** definition and recurses with the concept's parameter bound to the argument. Depth is bounded at eight (a
concept may name itself). Two facts the tree decided, both measured:

* a concept's `template <…>` is a **sibling** of its `ConceptDecl` (`Declaration > [TemplateDecl, ConceptDecl]`), so the
  parameter list comes from the enclosing declaration;
* a body naming another concept (`integral<T>`) is the case needing recursion — and the case `substitute` was breaking.

```text
describe(1.5)   integral<double> && is_signed<double>  = false && false → the floating overload
describe(-3)    …                                      = true  && true  → the signed overload
```

## Five more traits, and which type the qualifier is about

```text
is_reference   int&   int&&      a trailing `&`, one or two
is_const       const int         the qualifier on the *topmost* type
is_volatile    volatile int      the same
is_array       int[4]  int[]     a `[` in the spelling
is_function    int(int)          a `(` in the spelling, which no object type has
```

The first version of `is_const` read the leading `const` of `const int*` as the pointer being const — it is not: that
`const` belongs to the pointee. The rule, stated once and read before the decorators are peeled:

```text
const int        const                     → true
const int*       pointer to const int      → false
const int&       reference to const int    → false
int* const       const pointer             → true
```

**Still refused:** `is_class` and `is_enum` need a symbol table (a name is a class, an enum or a typedef), and
`is_base_of`/`is_convertible`/`is_trivially_copyable` are relations between two types. They answer `None`, and a test pins
that `is_class_v<T>` stays unanswerable rather than quietly becoming `false`.

## The next shapes: `std::optional`'s members are wrong in two different ways

The techniques fixture grew a fold, a capturing lambda, a generic lambda, `std::function`, `std::optional` and
`std::variant`, and the probe asks about them:

```text
FAIL  maybe.has_value()     68 offered   `has_value` missing from ["append", "assign", "at", "c_str", …]
FAIL  maybe.value()         68 offered   the same string list
FAIL  number.value_or(0)     0 offered   `value_or` is not reachable
PASS  maybe->size()         68 offered   `size` is in the list
```

* **`std::optional<std::string>` answers `std::string`'s members for `.`** — while `->` correctly goes through
  `operator->`. Two reads of one object disagree, and the `.` one is wrong.
* **`std::optional<int>` answers nothing** — the same class, a different argument. That emptiness is also where the false
  `param-type-check` comes from (*"this passes `0` where `_Ty2&&` is declared for `value_or`"*).
* the false finding's path could not be traced: `convertible_to(int, _Ty2&&)` is `Unknown`, the `Unknown` arm is written as
  silence, and `a_class_can_be_built_from` is never asked about `_Ty2`. Recorded with those three measurements rather than
  a guess.

## A `.` on a class that declares `operator->` lists that class's own members

Four member rows were red and all four came from one step: `the_class_after_the_operator` followed a "pointer-like" class
through `element_type`/`value_type`, so a `.` typed where `->` was meant still offered the pointee's members. Right for
`shared_ptr`; **wrong for `std::optional`**, which declares both `operator->` and `value_type`:

```text
before                                        after
maybe.has_value()   68 offered (the string's)  15 offered (optional's own)
maybe.value()       68 offered (the string's)  15 offered
number.value_or(0)   0 offered                 15 offered
maybe->size()       68 offered                 68 offered  (an arrow, unchanged)
```

The rule is now **`element_type`** — the standard's own pointer protocol, what `unique_ptr`/`shared_ptr`/`weak_ptr` declare
— and not `value_type`, which every container and wrapper declares. The fixture that pinned the old rule was rewritten to
pin the new one, with the trade stated in its doc comment: the smart-pointer convenience is kept for the classes that take
part in the protocol, and shapes that merely look similar stop borrowing another type's members.

`cpp_project` now reads **9/9 members, 7/7 types**; the C corpus is unchanged (2/2, 3/3, no findings). The one finding left
in the C++ project is the `param-type-check` on `value_or(0)`, with its three measurements recorded.

## The false `param-type-check` on `value_or(0)`: a template parameter is not a class to ask about

The trace named the step:

```text
[param-candidate] name="value_or" parameter="_Ty2&&" relation=Unknown(…) conversion=Refused
```

The relation answered `Unknown` — right — and then `sema::convert::conversion` asked the **class question** about `_Ty2`, a
name nothing declares, and got `Some(false)` ("no constructor could take this"), which turned *cannot tell* into a
**refusal**. Two candidates, both refused, `every_candidate_disagrees` stayed true, and a valid call was reported.

The fix is the convention `sema::constraint` already uses: **a parameter written as a template parameter is not a class** —
one capital letter (`T`, `U`, `V`), or the standard libraries' underscore-prefixed spelling (`_Ty2`, `_Up`, `_Elem`), which a
project's own classes do not use. `conversion` answers `Unknown` for those before asking anything.

Measured after: `cpp_project/src/techniques.cpp` — **0 findings**. The project's own baseline is now
**0 parse errors, 0 findings, 9/9 members, 7/7 types**; the C corpus is unchanged (2/2, 3/3, 0 findings).

## Deduced return types: same file works, through a header it is reached and cannot complete

```cpp
class Box {
    T value_;
    auto copy_of() const { return value_; }   // `auto`, decided by the body
};
Box<int> box(7);
auto copied = box.copy_of();
```

```text
PASS  same file                                          → `int`
PASS  same file, with a constructor and public:/private: → `int`
FAIL  member declared in the header                      → unknown
```

`type_a_return_states_with` could only type a **parameter** named by a `return`; a data member (`return value_;`) was
unanswerable until a reader for the field's declared type was added. The caller's substitutions then turn `T` into `int`.

The header path is **not** fixed, and the trace localises it: the fact is found (`copy_of`, offset 4855), `declaring` hands
back the header, `definition_at` finds a binding there — and `deduced_returns_of_substituted` answers `None`. The deduction
is reached and cannot finish, in a tree whose offsets come from a *rendering* while the fact's come from the *written* file.

The hook was written, measured and reverted: it did not fix the cross-file case, and its scan sits on the hot path that types
every deduced return. `the_reported_file_is_answered`'s budget is 30 s and it runs 20–35 s on a loaded machine — the failures
seen today were load (the suite passes with the hook stashed and with it in place), so the two probe rows stay red as a
**measurement**, not a regression.

## A member's deduced return type, through a header

```cpp
// a.h
class Box { T value_; auto copy_of() const { return value_; } auto doubled() const { return value_ + value_; } };
// a.cpp
Box<int> box(7);
auto copied = box.copy_of();     // `int` now
auto doubled = box.doubled();    // and the arithmetic one
```

Three things had to be true:

1. **`auto` is not a return type.** The index records `auto`, and `what_a_call_has_in` parsed it into a *type called `auto`* —
   an answer that reads like one. A placeholder answers `None` now, which is what lets the caller deduce from the body.
2. **A `return` can name a data member.** `type_a_return_states_with` typed parameters only, so `return value_;` was
   unanswerable; `a_data_member_type` reads the field's type from the class body, with a one-level walk, no allocation until a
   name matches, and a 64-member bound — this runs for every deduced return whose name is not a parameter, and an unbounded
   scan there is 8 s versus 35 s on the handshake suite.
3. **The fact's offset is a file offset; the tree speaks the reading.** The fallback that reads a body from the *declaring*
   file looked the offset up directly — right only when the two coincide. It translates with `reading_offset_of`, tries the raw
   offset second (a `#if` branch nobody takes), and accepts a binding only if it carries the fact's name.

```text
a_members_deduced_return_type_is_read_through_a_header   red before, green after
cpp_project: 0 errors, 0 findings, 9/9 members, 9/9 types  (was 7/9)
```

## The shapes around the edges: lambdas, structured bindings, `?:`, function pointers

Every one of these was **measured red first**, in the probe's own file layout, and each turned out to be a different layer:

| shape | before | after | what was wrong |
| --- | --- | --- | --- |
| `auto copied = box.copy_of()` | unknown | `int` | `auto` was parsed as a type *called* `auto`, and a `return` naming a data member was unreadable |
| `auto total = sum_of(1, 2, 3, 4)` | unknown | `int` | a fold is `ParenExpr > BinaryExpr > [pack, FoldExpr]`, and a *pack* is no single substitution |
| `auto result = doubler(3)` | unknown | `int` | a lambda's type is unnamed; the body is reachable through the variable it was stored in |
| `auto [n, f] = pair` | `int()` for both | `int`, `double` | the `=` of a structured binding is inside the `InitDeclarator`, which the pattern reader did not look in |
| `auto widened = c ? 1 : 2.0` | unknown | `double` | no `TernaryExpr` arm; the widening table from the fold is shared |
| `int (*fp)(int) = negate; fp(3)` | *"`fp` is not declared"* | `int` | a parenthesised declarator was called unnamed, then read as a function, and its type lost the parameter list |
| `auto cut = text.substr(1, 2)` | `basic_string` | `basic_string<char, …>` | an injected-class-name stands for the class *with its arguments* |

Two of those were **wrong answers rather than missing ones**, which is the worse failure: `int()` is a type-shaped lie, and
`basic_string` with no arguments is a different type from the one the declaration means. The rule applied throughout: an
answer this layer cannot build is `None`, and a spelling it *can* build is built completely or not at all.

The function-pointer case is worth its own line, because the false diagnostic was the visible half: `int (*fp)(int) = negate;`
made `fp` **undeclared in the function that declares it** — the AST's name reader answers `None` for a parenthesised
declarator, so the walker decided the declaration was unnamed and never read the declarator at all. The scope tree had no `fp`,
and the undeclared-identifier check reported the file's own variable. Three separate fixes, each verified by a trace:
`is_unnamed_declaration` consults the declarators, a nested declarator that names nothing no longer ends the walk, and a
declarator is read as a function only when it is **not** `(*`/`(&`-parenthesised — a qualified definition's declarator is a
bare parameter list and must stay a function.

An existing inlay test had **pinned the old boundary** (`void (*f(int a))(int b);` "is not bound, so nothing resolves the
call"). It is now green in the opposite direction: `f` resolves, and the hint reads `f`'s own parameter `a`, not the `b` of the
function it returns — exactly what its own comment predicted would work the day the binding existed.

## Overload resolution by the call's own arguments, and the report card it produced

The probe now asks **45 type questions and 10 member questions** of a real C++ project and its answer is 44/45 and 10/10, with
**0 findings** and **0 parse errors**. What closed the last big gap:

```cpp
std::to_string(count)        → `string`
std::get<int>(either)        → `int`
std::make_unique<Closing>()  → `unique_ptr<Closing>`
```

All three were `Ambiguous`: a name declared more than once answers the *name* query, which is right for "where is this
declared" and wrong for "what does this call return". `the_return_the_overloads_agree_on` asks each candidate two questions —
can it be called this way, and does it spell the same type once the written template arguments are substituted — and answers
only when every survivor agrees. Four rules make those questions answerable, and each came from a measurement:

* a **deleted** function is not a candidate (`DELETED` is already a fact bit; `std::make_unique`'s deleted overload was making
  the set disagree);
* a **pack** is recognised from the written list, because `parameters_in` strips the `...` (`(_Types&&... _Args)` comes back as
  one parameter);
* a return spelling that **still names one of the candidate's own parameters** could not be substituted and is dropped rather
  than compared;
* a declaration with **no recorded parameter list** has no vote.

## Two false findings removed from the member check

```text
concept sized = requires(T value) { value.size(); };   ← "`value` has no member `size`"
widget.raw()                                           ← "`widget` has no member `raw`"   (CRTP base)
the_widget().stored                                    ← "`the_widget()` has no member `stored`"
```

**A requires-expression asks whether the member is there** — a member missing from the hypothetical `T` is exactly what the
concept tests for, so nothing inside a `RequiresExpr` is reported. **A class name two files declare is not evidence**: the
project has two `Widget`s and the query answered with whichever the index filed first, so the check now asks how many visible
declarations of the name there are and judges none of them when it is more than one. Both rules only ever remove a finding.

## Known gaps, as measurements rather than as prose

```text
FAIL  auto raw = widget.raw();   CRTP: `raw` is `Counted<Widget>`'s member. The member *list* finds it (10/10 rows
                                 include it), but the single-member query answers NotDeclaredHere when two classes share
                                 the name — the same two-`Widget` collision, one layer below where it was fixed.
```

An **overload set that disagrees after narrowing** answers nothing on purpose: choosing between those candidates is overload
*ranking* (conversions, template partial ordering), which this layer does not do.

## Why the CRTP row cannot be fixed where it looks fixable — a measured constraint

```cpp
template <typename Derived> struct Counted { int raw() const { … } };
struct Widget : Counted<Widget> { int stored; };
Widget widget;  auto raw = widget.raw();      // no type
```

The member *list* finds `raw` (depth 1, from `Counted`), so `members_of("Widget")` is right and the completion offers it. The
single-member query, `member_fact("Widget", "raw")`, answers `NotDeclaredHere`, and the reason is not the base walk:

* the project has **two** `Widget` classes — one in `techniques.h`, one in `main.cpp` — and a bare spelling resolves to
  whichever the index filed first;
* the natural disambiguator is the file the **object's type was declared in**, and that is what this pipeline cannot give: the
  declaration's file, as `type_of` reports it, is the file the *expression* was read in — because a `.cpp`'s rendering has its
  headers' text **inlined**, so the header's identity is already gone by the time the type is answered. Measured with the
  candidate fix in place: `member_fact("Widget", "raw")` arrived with `from = techniques.cpp`, and that file's members for
  `Widget` were `["identifier", "size"]` — the *other* class's, read out of the same rendering.

That was tried, measured and reverted rather than shipped: threading the type's file into the member query changed no answer
and added a parameter to eight call sites. The fix this needs is one layer down — the type has to carry the file its
declaration was *written* in, which means the rendering has to keep the span table's file boundaries by the time a type is
built. That is a real piece of work and it belongs to its own change, not to a member query.

## A header sweep: the false findings it found, and what is still there

A new ignored test (`tests/headers_sweep.rs`) indexes sixty of MSVC's headers and runs **every check** over them. Valid code,
so every finding is a defect of this layer's — and it is the only way to find defects that a three-file project cannot show:

```text
CPPLS_HEADERS="…\VC\Tools\MSVC\14.35.32215\include" \
  cargo test -p cpp_code_analysis --test headers_sweep -- --ignored --nocapture
```

```text
before   {AccessInvisible: 7, AssignTypeCheck: 1, CStyleCast: 138, ElseAfterReturn: 2, MacroParentheses: 8,
          Redefinition: 5, UndefinedName: 11, UseNullptr: 26}
after    {AccessInvisible: 7, AssignTypeCheck: 1, CStyleCast: 138, ElseAfterReturn: 2, MacroParentheses: 8,
          Redefinition: 5, UndefinedName: 2, UseNullptr: 26}
```

**`UndefinedName` fell from 11 to 2**, all nine from one COM header, and the cause was worth more than the nine: an
**out-of-class definition's function scope was parented to the file**, so `void W::f() { a = 1; }` could not see `a` even
when `W` was declared three lines above. That is the shape every C++ project is written in. See the commit for the tree
before and after (`File{W}(Class{a,f} Fn)` → `File{W}(Class{a,f}(Fn))`).

**What is still there**, with what it would take:

* `CStyleCast` 138, `UseNullptr` 26, `MacroParentheses` 8, `ElseAfterReturn` 2 — **style** rules. A vendor's headers are not
  written to a project's style, and an editor offering these on `<vector>` is noise. A real project turns them off for
  system headers; whether this layer should decide that for the user is a product question, not a defect.
* `AccessInvisible` 7 — all in `comutil.h`, all about `m_Data`/`m_wstr` in `_bstr_t`'s own member definitions. Its members are
  **private**, so this is the same family as the last point: the code *is* the class's own.
* `Redefinition` 5 — `_Ptr`, `_Src`, `_Destination_tuple` in three `#if`-heavy headers. The check only judges function, block
  and lambda scopes, and the shapes were not reproducible from a fixture (`two_functions_that_both_name_one_variable` passes),
  so the next step is a trace on the header itself rather than another guess.
* `UndefinedName` 2 — the two that remain in `comutil.h`.
