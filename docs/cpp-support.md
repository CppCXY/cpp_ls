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
