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

Two instruments ask the two questions: the probe below for a whole project's findings, and `cpp_inspect` for one
question at a time (the hover and definition rates in the table were measured by a probe that has since been deleted —
`cpp_inspect hover`/`define` answer the same questions, one position per run or many through `batch`):

```text
$env:CPPLS_C_PROJECT = "E:\corpora\libuv"
cargo test --release -p cpp_ls --test c_diagnostics_probe -- --ignored --nocapture   # findings, file by file

cargo run --release -p cpp_inspect -- --project E:\corpora\libuv --file src/win/core.c hover --needle WSAGetLastError
```

| Question | Answer on libuv 1.53.1 (Windows build, 40 files) |
| --- | --- |
| Does it parse? | **yes** — 0 `syntax-error`, 40 of 40 reports answered, 33 s |
| Hover | 466 / 526 (89 %), p50 12 ms, p95 4.3 s |
| Definition | 403 / 526 (77 %), p50 0 ms, p95 1.5 s |
| Hints | 85 `braces-around-statements`, 12 `else-after-return` — libuv writes `if (!handle) return UV_EINVAL;` by choice, so these are opinions the registry already marks Hint |
| Errors | **0** — the twelve false positives this table used to carry are §7.1 and §7.2 below, both fixed |

### 7.1 `undefined-name` on a name a header declares — fixed

`src/win/tcp.c:1627` said `` `uv_translate_sys_error` is not declared: nothing this file can see names it `` — and
`include/uv.h:452` declares it at file scope. `crates/cpp_code_analysis/tests/c_name_lookup_probe.rs` showed the
header was indexed and its summary held **21 declarations of 563**, all from lines 69–103.

The parser was not the cause: `cpp_dump` on the file's own bytes reported 0 errors in both dialects. The **enum** was:

```c
typedef enum {
#define XX(code, _) UV_ ## code = UV__ ## code,
  UV_ERRNO_MAP(XX)
#undef XX
  UV_ERRNO_MAX = UV__EOF - 1
} uv_errno_t;
```

Two rules were missing, and both are the X-macro spelling every C library writes:

* **an enumerator whose name takes an argument list** — `UV_ERRNO_MAP(XX)` is one enumerator *here* and many after
  expansion; without consuming the group the `(` ended the enumerator and the enum body ended there;
* **no comma between that invocation and the next enumerator** — the commas live in the macro's body, not in this
  file, so the token stream has `UV_ERRNO_MAP(XX)` `#undef` `UV_ERRNO_MAX` with no `,`; the loop now continues when
  the enumerator it just read took an argument list and the next token is a name.

Measured on `include/uv.h`: the summary went from **21 declarations to 788** (locals dropped) / 1 464 (kept), and
`uv_translate_sys_error` is among them. libuv 1.53.1's Windows build went from **11 errors to 3**, all three of them
`WSAGetLastError`.

Fixture: `crates/cpp_parser/tests/c_language.rs::an_x_macro_enum_parses_as_one_enumeration`.

**And the same shape one construct along** — `WSAGetLastError`, declared in MinGW's `winsock2.h:1049`, was still
missing: that file's summary held **206 of 313** declarations and the walk stopped at line 899,

```c
typedef unsigned __LONG32 (WSAAPI *LPFN_INET_ADDR)(const char *cp);
```

which is `typedef unsigned long (__stdcall *F)(const char *)` before the macros are expanded. Read **unpreprocessed**,
`unsigned __LONG32` is a builtin and a name, the `(...)` that follows is that name's parameter list, and the second
`(...)` was refused — `Widget w(g())` is the same shape and *must* be refused, because there it is a call. The rule now
distinguishes them by what came before: a group **straight after the declarator's name** still fails the declaration
(so the expression grammar reads `w(g())`), while a group **after a parameter list** is one more suffix. Measured:
`winsock2.h`'s summary went from **206 declarations to 809** (the file ends at line 1324), and `WSAGetLastError`
resolves.

**The cache had to be told.** A stored summary is keyed by `READING_FINGERPRINT`, a hash of `cpp_parser::VERSION`,
the crate version, `FORMAT_VERSION` and the codec version — none of which move for a **grammar edit in a working tree**,
so the first re-measurement after the fix still read the old 206. That is what `FORMAT_VERSION` is for (its own module
comment says so); it moved **44 → 45** with this change, which invalidates every summary built by the old grammar
rather than only this project's.

**Final measurement of the objective on libuv 1.53.1's Windows build (40 files from the CMake database)**:

| Question | Answer |
| --- | --- |
| Does it parse? | **yes** — 0 `syntax-error`, 40 of 40 reports answered |
| Errors | **0** (was 12) |
| Warnings | **0** |
| Hints | 97 — 85 `braces-around-statements`, 12 `else-after-return`, both `Hint` by the registry's own table |
| Hover / definition | 466/526 (89 %), 403/526 (77 %) — unchanged by these fixes, still the next usability question |

Fixtures: `sema/types.rs`'s `deducing_specifiers`, `sema/check/test/assign_type_check.rs`,
`crates/cpp_parser/tests/c_language.rs`'s `an_x_macro_enum_parses_as_one_enumeration` and
`a_second_parameter_list_after_a_macro_type_is_one_declarator`. `c_name_lookup_probe` grew the "late facts" print that
found both stopping points: a summary that holds a prefix of its declarations is a construct the walk gave up at, and
the construct is the **last fact's line**. 
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

### 8.4 The instruments, after the cleanup

The measurement probes this document and `docs/open-questions.md` name were **deleted**, and the remaining test files
were merged into one target per crate: 244 test binaries linked in ~431 s of gate time became 7 linked in **30 s**,
while the tests themselves (≈4 900) run in under 25 s. That is the whole reason: a probe marked `#[ignore]` still costs
a **link**, and there were 91 of them.

What is left:

* `cpp_ls/tests/c_diagnostics_probe.rs` and `cpp_code_analysis/tests/c_name_lookup_probe.rs` — the two C-project
  instruments this document's §7 and §8 are measured with, kept as their own targets because they are run explicitly
  (`--ignored`) and their output is a report rather than an assertion;
* `crates/cpp_inspect` — one question per run, as JSON, for the hover/definition/members/type/facts questions the
  deleted probes used to compile a file for. See its README.

A document that names a probe path is therefore naming the instrument **as it was when the number was taken**. The
numbers stand; the command no longer compiles.

## 8. The second project: redis 8.x, with no compilation database

`c_diagnostics_probe` now **scans a project that has no `compile_commands.json`** (the server does the same: a checkout
nobody built is the ordinary case), so redis is a measurement rather than an excuse. 240 `.c`/`.h` files under
`E:\corpora\redis`, `didOpen` + `textDocument/diagnostic` each, with `languageId: "c"`.

### 8.1 The first measurement, and the one file that owned it

| | |
| --- | --- |
| reports answered | 240 of 240 |
| errors | **904**, of which **501 syntax errors** |
| warnings | 642 |
| hints | 924 |

**952 of the 1002 syntax errors were in `src/config.c` alone.** The first is at line 4197,
`standardConfig static_configs[] = {`, and the rendering shows why:

```text
... static_configs[] = { default, is_valid, apply) { .name = (name), .alias = (alias), ... } ( "rdbchecksum", ... )
```

The macro is redis's own:

```c
#define createBoolConfig(name, alias, flags, config_addr, default, is_valid, apply) { \
    embedCommonConfig(name, alias, flags) \
    embedConfigConverterConfig(boolConfigInit, boolConfigSet, ...) \
    ...
```

`parse_parameters` accepted an `Identifier` as a parameter name and nothing else. `default` is a **keyword**, so the
parameter list was cut there, the rest of the list became the macro's *body*, and the macro became **object-like** —
every `createBoolConfig(…)` in the file then stayed unexpanded text. The preprocessor's grammar has no keywords at all
(C11 §6.10.3: a parameter is an *identifier*, and `default` is one at that phase), and `could_be_a_macro_name` — the
rule the **macro's own name** already used for `#define true 1` — is the right test for a parameter too.

Fixed in `preprocess/macros.rs::parse_parameters`; `FORMAT_VERSION` moved 45 → 46, because the wreckage is what a
stored summary holds. Differential fixture:
`crates/cpp_toolchain/tests/preprocessor_against_the_compiler.rs::a_parameter_spelled_with_a_keyword_expands_as_the_compiler_says`
(compared against `gcc -E`'s own token stream).

### 8.2 The second measurement

| | before | after |
| --- | --- | --- |
| errors | 904 | **427** |
| syntax errors | **501** | **4** |
| warnings | 642 | 646 |
| hints | 924 | 977 |

What is left, by class, and where each one stands — none of these is skipped, and the ones that are not fixed yet say
what would settle them:

| code | count | what it is |
| --- | --- | --- |
| `undefined-name` | 391 | **Not a defect yet**: no compilation database means no `-I` and no defines, so `va_list`, `pthread_*` and the system headers are invisible. The next step is a synthesized database from redis's own `Makefile` flags (`-std=gnu11 -Isrc -I../deps/…`), which is what `bear` would write. |
| `missing-include` / `include-not-found` | 273 / 216 | The same absence, seen from the include edge. |
| `cstyle-cast` | 73 | **Every instance is in a `.h`.** A header's family is unknown when nothing configures it, and `DiagnosticCode::is_cpp_only`'s rule is "not told is not C" — so a C++ rule fires in a C project's headers. The principled fix is the header's **includers** (or the editor's `languageId`, which `didOpen` carries and the server ignores today); it is the next round's work, and it is recorded here rather than worked around by guessing from the extension. |
| `macro-parentheses` | 71 | A real style finding about redis's own macros (`atomicvar.h`), matched against gcc's `-Wparentheses` behaviour in §7's spirit. Not yet triaged line by line. |
| `assign-type-check` | 18 | All in `src/eval.c`. The rule was fixed for libuv in §7.2 and this is a **different** shape; triage next round. |
| `assignment-in-condition` | 12 | gcc says `-Wparentheses` here too; not triaged. |
| `return-type-mismatch` / `param-type-check` / `dangling-return` / `macro-redefined` | 8 / 5 / 1 / 1 | Not triaged. |
| `syntax-error` | 4 | Two files: `src/connection.h:431`, `src/sentinel.c:26` (plus two in `server.c`/`server.h`). Next round. |

### 8.3 A header the client calls C is not judged by C++ rules — fixed

The 73 `cstyle-cast` findings in the table above were **all in `.h` files**, and none of them was about a cast the
language wants rewritten: `(int)p` is a C-style cast in C++ and *the* cast in C. The mechanism was a chain of two
defaults:

* `CompilerConfig::for_file` deliberately does not decide a **header's** family from its extension — a header is
  included by whichever language includes it — so a `.h` takes the **project-wide** standard, and an editor's default
  for that is `c++23`;
* `DiagnosticCode::is_cpp_only`'s rule is therefore "not told is not C", which was true of the project and false of
  the file: `redis` is a C project, and its headers are included by C files only.

The missing statement is one the client already sends: `didOpen`'s **`languageId`**, the editor's own language mode,
which the server was discarding. It is now kept per open document (`Session::did_open_as`, and `did_close` forgets it),
and `Session::config_for_file` uses it for a file the **compilation database does not name** — a database entry is a
fact about the file and still wins, while the project-wide default is not. `didOpen` is the only handler with a
`languageId`; a change or a save carries text, not a language, and leaves the answer alone.

Measured on redis 8.x (cold cache, 240 files): **`cstyle-cast` 73 → 0**, warnings 648 → 573, errors unchanged at 427
(the rest are the absent database's, §8.2). Fixture:
`crates/cpp_ls/tests/a_header_language.rs` — the same header opened twice, `languageId: "cpp"` (finding present) and
`languageId: "c"` (absent), both through the real server.

`FORMAT_VERSION` moved 46 → 47: a summary built while the header was read as C++ is a different reading, and
`context_hash` does not cover the client's answer.

The point of the table is that the **next** measurement is a database-shaped one: redis without include paths cannot
distinguish "the analysis missed a name" from "nobody told it where the name is", and the numbers above say so
explicitly instead of counting them as defects.
