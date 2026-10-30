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
