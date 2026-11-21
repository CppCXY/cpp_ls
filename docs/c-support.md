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

**One row is read in every standard**, because the compilers do: `_BitInt(N)` is C23's, and gcc 15 accepts
`struct bits { unsigned _BitInt(3) three : 3; _BitInt(13) signed_wide : 13; _BitInt(5) plain; };` with `-std=c99`,
`-std=c11`, `-std=c17` **and** `-std=gnu89` (exit 0, no diagnostic), while clang 18 accepts it in `-std=c++23`. A reader
that gated it on C23 misread a legal declaration as a call whenever the standard was not detected — no compile database,
a header opened on its own — and the struct came out with no members at all. The gate is open in every standard the
server knows; the detector around it still requires a parenthesised width followed by an identifier, so a variable named
`_BitInt` is untouched.

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
| `textDocument/hover` | macros first (a `#define` **is** its value), then the declaration (signature/type), then the type of the expression; the comment next to the declaration comes with it, its Doxygen inline commands (`@c`, `@p`) rendered; a struct also says what it costs in memory, and an attribute or a macro written **against** a declaration is drawn above its signature with the macro expanded — a macro that is a statement of its own is not |
| `textDocument/signatureHelp` | the list of overloads with `activeParameter` — the position of the argument being typed |
| `textDocument/definition` | the declaration of a macro, type, function or variable, across files through the index |
| `textDocument/implementation` | a function declaration → its definition, across files |
| `textDocument/typeDefinition` | a `typedef`/`struct` name → the definition of its type |
| `textDocument/references` | a macro's uses **exactly** — the macro environment decides each one, including those reached under a conditional `#include` or `#if` (shown, and refused by rename); an ordinary name through the same query the code lens counts, so the number a lens shows and the places a click reveals cannot disagree |
| `textDocument/rename` + `prepareRename` | a macro's uses, or an ordinary name's — written as one `WorkspaceEdit` over the list Find References answers; refused with the protocol's `null` while the index still has work (half a rename is a broken build) or when the new spelling is not one identifier |
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
| `textDocument/formatting`, `rangeFormatting`, `willSaveWaitUntil` | whole-file and range formatting — clang-format from `PATH` by default (`[format] formatter`), or the built-in `cpp_formatter` |
| `workspace/didChangeConfiguration` | `.cppls.toml` and editor settings — see [`configuration.md`](configuration.md) |

## 6. Diagnostics

The registry is the one [`cpp-support.md`](cpp-support.md) §6 lists. A rule that only means something in C++ — access
control, `override`/`final`, concepts, contracts, `auto`, the Unreal rules — is **skipped** for a C file rather than
run and silenced, so it costs nothing and cannot fire. Twenty-one codes can fire in C:

| Code | Severity | Meaning |
| --- | --- | --- |
| `syntax-error` | Error | the text does not parse |
| `param-type-check` | Error | an argument's type cannot go to the parameter |
| `assign-type-check` | Error | an assignment or initialisation does not fit the type |
| `undefined-field` | Error | the member is not in the `struct`/`union` |
| `undefined-name` | Error | the name is not declared |
| `redefinition` | Error | the same entity is defined twice in one scope |
| `argument-count` | Error | the call passes the wrong number of arguments |
| `return-type-mismatch` | Error | a `return` does not fit the declared type |
| `missing-return` | Error | a function that promises a value can reach its end |
| `dangling-return` | Error | a pointer to something that dies when the function returns — a local, or a pointer a local handed out |
| `use-after-free` | Error | a pointer read after the memory it refers to was released — by `free`/`realloc`, or by a call to a function whose fact says it frees that argument |
| `macro-redefined` | Warning | the same macro is redefined with a different body |
| `include-not-found` | Warning | the `#include` was not found — a warning, because the search is ours rather than the compiler's |
| `missing-include` | Warning | a type is used and no include in this file declares it |
| `unnecessary-branch` | Warning | a branch that cannot run |
| `assignment-in-condition` | Warning | an assignment inside a condition |
| `macro-parentheses` | Warning | a macro body without parentheses of its own |
| `unused` | Hint | a local nothing reads (`[[maybe_unused]]` and RAII-shaped declarations are not reported) |
| `braces-around-statements` | Hint | a single-statement branch without braces |
| `simplify-boolean-expr` | Hint | a boolean expression that can be simplified |
| `else-after-return` | Hint | an `else` after a `return` |

`[diagnostics].disable` turns one off by name, and the severities are the registry's own — a client that shows only
errors shows exactly the rows marked Error here.

## 7. Measured on a real C project: libuv 1.53.1

The first C project measured end to end, because "usable for C" is a claim about a program rather than about a grammar.
libuv is the right first subject: it is C11, it is `typedef struct` and macros all the way down, and its Windows build is
one compile database over 40 files (`src/*.c` and `src/win/*.c`).

```text
git clone --depth 1 https://github.com/libuv/libuv          # 1.53.1
cmake -S . -B build -G Ninja -DCMAKE_C_COMPILER=gcc \
      -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -DLIBUV_BUILD_TESTS=OFF -DLIBUV_BUILD_BENCH=OFF
# 80 entries (the shared and static libraries each list every file), 40 distinct files,
# -std=gnu11 -Iinclude -Isrc -DBUILDING_UV_SHARED=1 -DWIN32_LEAN_AND_MEAN -D_WIN32_WINNT=0x0A00
```

Two probes ask the two questions, over the protocol against the real binary:

```text
$env:CPPLS_C_PROJECT = "E:\corpora\libuv"
cargo test --release -p cpp_ls --test c_diagnostics_probe -- --ignored --nocapture   # findings, file by file
$env:CPPLS_NEOVIM = "E:\corpora\libuv"; $env:CPPLS_USABILITY_FILES = "92"
cargo test --release -p cpp_ls --test neovim_usability -- --ignored --nocapture     # hover / definition rates
```

| Question | Answer on libuv 1.53.1 (Windows build, 40 files) |
| --- | --- |
| Does it parse? | **yes** — 0 `syntax-error`, 40 of 40 reports answered, 33 s |
| Hover | 466 / 526 (89 %), p50 12 ms, p95 4.3 s |
| Definition | 403 / 526 (77 %), p50 0 ms, p95 1.5 s |
| Hints | 85 `braces-around-statements`, 12 `else-after-return` — libuv writes `if (!handle) return UV_EINVAL;` by choice, so these are opinions the registry already marks Hint |
| Errors | **12, and they are false positives** — 11 `undefined-name` and 1 `assign-type-check`, below |

### 7.1 `undefined-name` on a name a header declares

`src/win/tcp.c:1627` says `` `uv_translate_sys_error` is not declared: nothing this file can see names it `` — and
`include/uv.h:452` declares it at file scope, on a line `src/win/tcp.c` reaches through its own `#include "uv.h"`.

Measured, not inferred (`crates/cpp_code_analysis/tests/c_name_lookup_probe.rs`):

```text
[probe] index holds a summary for include/uv.h: true
[probe] that summary: 20 declaration(s), 9 include(s), 61 macro(s)
[probe] first 12 names: ["uv_addrinfo_t", "uv_addrinfo_s", "uv_sockaddr_t", ..., "sockaddr"]
[probe] does the summary name uv_translate_sys_error: false
```

So the header is indexed and its summary holds **20 declarations of the ~800 it has** — all of them from lines 69–79.
The parser is not the fault: `cpp_dump` on `include/uv.h` reports **0 errors**, and the name is in the tree at offset
20306. What the index parses is the **rendering**, and that rendering **starts at line 75** (the `#else` branch of
`#if defined(UV_FREERTOS)`): everything before it, from the include guard to `UV_EXTERN`'s definition, is missing from the
text — and the summary stops right after those typedefs.

```text
[probe] E:\corpora\libuv\src\win\core.c can see uv_translate_sys_error: Unknown(NotDeclaredHere("uv_translate_sys_error"))
[probe] uv_translate_sys_error (from the header's own file): Unknown(NotDeclaredHere("uv_translate_sys_error"))
```

A reduced fixture does **not** reproduce it (a header with a guard, an `#if` with two branches and declarations after it
resolves), so the trigger is in `uv.h`'s own structure: a guard, `UV_EXTERN` defined under `#ifndef UV_EXTERN` +
`#ifdef _WIN32`, then `#if defined(UV_FREERTOS) … #else … #endif`, then `#elif defined(_WIN32) # include "uv/win.h"`.
That is the next thing to fix, and this section is the measurement it will be checked against.

### 7.2 `assign-type-check` and a type the compiler deduces — fixed

`src/uv-common.c:576` said `` `__atomic_exchange_val` is initialised with a value that does not convert to `void` ``.
The name is not the reader's: GCC's `atomic_exchange_explicit` expands a statement expression whose second line is

```c
__typeof__ ((void)0, *__atomic_exchange_ptr) __atomic_exchange_val = (VAL);
```

and the **type reader** spelled that type `void` — the `BuiltinType` inside the `typeof`'s own argument won the walk
(`read_specifiers_named`) — so the declaration was judged against a type nobody wrote. Measured with a probe on the
reader itself: `__typeof__ ((void)0, *p)` → `Builtin { spelling: "void" }`, `__typeof__(p)` and `decltype(p)` →
`Builtin { spelling: "__typeof__(p)" }`.

The fix is in the layer that was wrong, and it is a question about a **keyword kind** rather than about a spelling:
`read_specifiers_named` answers "no written type" (the empty spelling, which records `type_of: None`) when the
specifier sequence begins with [`CppTokenKind::DecltypeKeyword`] — the kind the lexer already gives `decltype`,
`typeof`, `typeof_unqual`, `__typeof__` and `__typeof`. `auto` keeps its spelling on purpose: it is the marker the
deduction path reads (`is_a_deduced_type`), and a declaration that answered "no type" instead lost its deduction —
measured, two session tests went red when the rule included `AutoKeyword`.

Measured: libuv's Windows build went from **12 errors to 11** (0 syntax errors, 0 warnings, 97 hints, of which 85
`braces-around-statements` and 12 `else-after-return` — the registry marks both Hint, and libuv earns them by writing
`if (!handle) return UV_EINVAL;`). Fixtures: `sema/types.rs`'s `deducing_specifiers` (the `typeof` family spells no
type, `auto` keeps its spelling, an `int` is still `int`) and `sema/check/test/assign_type_check.rs` (a string in an
`int` is still reported; the assignment half still judges what the file wrote).
