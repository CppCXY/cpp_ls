# C support

What this server understands about C: how a file is identified as C, which language features it reads, what the
semantic layer can answer, what each LSP feature gives, and which diagnostics exist. Configuration and the limits are in
[`configuration.md`](configuration.md); the same reference for C++ is [`cpp-support.md`](cpp-support.md).

## 1. Language identification

| Item | Rule |
| --- | --- |
| Extension | `.c` is a C source; `.h` belongs to whichever language **includes** it (the C++ search list is the wider one, so a header read on its own is read as C++) |
| `-std=` | Wins over the extension: `c89`/`c99`/`c11`/`c17`/`c23`/`gnu17` and so on are C; `c++…` is C++ |
| Indexed extensions | `c cc cpp cxx c++ h hh hpp hxx h++ ipp inl tpp tcc cppm ixx`, plus anything in `[workspace].source_extensions` (for example `cuh`) |
| Header extensions | the header family above, plus `def mpp cuh` |
| Where files come from | `compile_commands.json` → `.cppls.toml` → the project scan; an `#include` no search path can place falls back to matching the tail of its path against the tree |

## 2. Standards and language features

Each construct is a **feature bit** opened by `-std=` or `[compile].standard`; a standard such as `c17` does not open the
C23 bits.

| Standard | Constructs accepted |
| --- | --- |
| C89 | the baseline: declarations before statements; no `//` comments, no `long long` |
| C99 | `//` comments, `long long`, `restrict`, `inline`, `_Bool`, variable-length arrays, flexible array members, compound literals, variadic macros, designated initialisers |
| C11 | `_Generic`, `_Atomic`, `_Static_assert`, `_Alignas`/`_Alignof`, `_Noreturn`, anonymous `struct`/`union`, `_Thread_local` |
| C17 | no syntax of its own (C11 plus the defect reports); the feature bits exist so the standard can be asked |
| C23 | `[[attribute]]`, `nullptr`, `constexpr`, `_BitInt(N)`, `auto` type inference, enumerations with a fixed underlying type, the empty initialiser `{}`, `#elifdef`/`#elifndef`, `typeof`/`typeof_unqual` |
| GNU extensions | statement expressions `({ … })`, label addresses `&&label`, `?:` with the middle operand omitted, `asm` blocks, nested functions, `__typeof__`/`__typeof`, `__attribute__`, `__builtin_*` calls |
| MSVC extensions | `__try`/`__except`/`__finally`, `__int8`…`__int64`, calling conventions such as `__cdecl`/`__stdcall` |

**Not supported**: C23's `#embed`. Every other C23 construct is in the table.

## 3. Preprocessing

| Ability | What it does |
| --- | --- |
| `#include` | angle and quote forms; search paths from the database, the configuration or the toolchain; a path nothing places falls back to tail matching; each directive records its resolution |
| `#define` | object-like and function-like, variadic (`...` and `__VA_ARGS__`), `#` stringification, `##` pasting, `__VA_OPT__` |
| `#undef` | recorded, so the macro environment is tracked point by point |
| Conditionals | `#if`/`#elif`/`#else`/`#endif`, `#ifdef`/`#ifndef`, C23's `#elifdef`/`#elifndef`; expressions are evaluated as integer constants |
| `#pragma`, `_Pragma` | recorded and ignored as the standard says |
| Predefined macros | `__STDC__` and `__STDC_VERSION__` follow the selected standard; the toolchain's own markers (`_MSC_VER`, `_WIN32`, `__GNUC__`, `__clang__`) follow the compiler |
| Macro environment | **the set of macros visible at each position** is recorded, and hover, completion, references and rename ask it rather than searching the text |

## 4. Semantic analysis

| Ability | What it answers |
| --- | --- |
| Scopes | blocks, function parameters and bodies, `struct`/`union`/`enum` (including C23 fixed underlying types), `typedef` names, `goto` labels, file scope; an anonymous `struct`/`union` member is promoted to the enclosing type |
| Types | built-in types, pointers, arrays, function pointers, bit-fields, `typedef` chains, qualifiers, compound literals, variable-length arrays |
| Names | visibility along the scope chain, `extern`/`static` redeclarations, `__typeof__`, names that a macro produced |
| Members | `struct`/`union` member lists (anonymous members, bit-fields, flexible array members); the right of `.`/`->` is answered from the object's type |
| Calls | argument count, argument types, variadic functions, return type; a call through a function pointer is answered from the pointer's type |
| Constants | `#if` expressions, array extents, enumerator values |
| The index | the cross-file declaration table, the `#include` graph, member-list caches, and the summary cache (`.cppls`) |

## 5. LSP features

| Feature | What it gives for C |
| --- | --- |
| `textDocument/completion` | names in scope (locals → the file → included headers), `.`/`->` members, macros, keywords, `#include` file names; with the range to replace, a `sortText` that is the analysis's own order, snippets when the client supports them, and `completionItem/resolve` for documentation |
| `textDocument/hover` | macros first (a `#define` **is** its value), then the declaration (signature/type), then the type of the expression; the comment next to the declaration comes with it |
| `textDocument/signatureHelp` | the list of overloads with `activeParameter` — the position of the argument being typed |
| `textDocument/definition` | the declaration of a macro, type, function or variable, across files through the index |
| `textDocument/implementation` | a function declaration → its definition, across files |
| `textDocument/typeDefinition` | a `typedef`/`struct` name → the definition of its type |
| `textDocument/references` | **exact references for macros**, including the ones reached under a conditional `#include` or `#if` (marked as uncertain); an ordinary name is not answered |
| `textDocument/rename` + `prepareRename` | **macros only**; an ordinary name is refused with the protocol's `null`, because editing half of a name's uses is a broken build written into the user's files |
| `textDocument/documentHighlight` | occurrences of the same name in the current file |
| `textDocument/documentSymbol` | the file's own declaration tree, **including what `#if 0` removes** (the reader is editing it), with a range for the whole declaration and one for the name |
| `workspace/symbol` | the declarations the project index holds, by name |
| `callHierarchy` | incoming and outgoing calls of a function |
| `textDocument/selectionRange` | the chain of syntax nodes around the cursor |
| `textDocument/foldingRange` | brace blocks, comments, `#region`, runs of `#include` |
| `textDocument/documentLink` | the path of an `#include`, as a link to the file |
| `textDocument/inlayHint` | parameter names at call sites |
| `textDocument/codeAction` | `source.include`: an unresolved name → an edit adding the header that declares it |
| `textDocument/semanticTokens` | the standard types (namespace, type, class, enum, typeParameter, parameter, variable, property, enumMember, function, method, macro, keyword, modifier, operator, decorator) and, inside string literals, `printf`/`scanf` format fields (`formatSpecifier`, `formatArgument`, `formatConversion`, `formatEscape`) |
| `textDocument/diagnostic` | **pull mode**: syntax errors plus the rules in §6 |
| `textDocument/formatting`, `rangeFormatting`, `willSaveWaitUntil` | whole-file and range formatting, reading the common subset of `.clang-format` |
| `workspace/didChangeConfiguration` | `.cppls.toml` and editor settings — see [`configuration.md`](configuration.md) |

## 6. Diagnostics

Twenty-six rules, each with a fixed severity; `[diagnostics].disable` turns one off by name. The rows marked **C++**
never fire in a file compiled as C — see [`configuration.md`](configuration.md) §5.

| Code | Severity | Meaning |
| --- | --- | --- |
| `syntax-error` | Error | the text does not parse |
| `param-type-check` | Error | an argument's type cannot go to the parameter |
| `assign-type-check` | Error | an assignment or initialisation does not fit the type |
| `undefined-field` | Error | the member is not in the type |
| `undefined-name` | Error | the name is not declared |
| `redefinition` | Error | the same entity is defined twice in one scope |
| `argument-count` | Error | the call passes the wrong number of arguments |
| `return-type-mismatch` | Error | a `return` does not fit the declared type |
| `include-not-found` | Error | the `#include` was not found |
| `access-invisible` | Error | a member reached from somewhere that may not reach it — **C++** |
| `missing-return` | Error | a function that promises a value can reach its end |
| `concept-not-satisfied` | Error | a constraint is not met — **C++** |
| `macro-redefined` | Warning | the same macro is redefined with a different body |
| `unnecessary-branch` | Warning | a branch that cannot run |
| `assignment-in-condition` | Warning | an assignment inside a condition |
| `c-style-cast` | Warning | a C-style cast — **C++** |
| `macro-parentheses` | Warning | a macro body without parentheses of its own |
| `unused` | Hint | a name nothing reads (tag: Unnecessary) |
| `braces-around-statements` | Hint | a single-statement branch without braces |
| `use-nullptr` | Hint | `0`/`NULL` used as a null pointer — **C++** |
| `use-override` | Hint | an overriding function without `override` — **C++** |
| `simplify-boolean-expr` | Hint | a boolean expression that can be simplified |
| `else-after-return` | Hint | an `else` after a `return` |
| `use-auto` | Hint | a declaration that could be written `auto` — **C++** |
| `implicit-bool-conversion` | Hint | an arithmetic value converted to `bool` — **C++** |
| `module-note` | Information | a note about a module's position — **C++**, not a complaint (no squiggle) |
