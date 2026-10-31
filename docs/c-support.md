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

## 5a. Macros, as the editor asks about them

A C program's most-read text is its macros, and the four questions a reader asks of one are answered as follows.

| Question | Where it is answered | What it does *not* need |
| --- | --- | --- |
| hover on a macro **use** | the #define's text, found from the file's own bytes | the rendering — the cook expanded the use away |
| hover on a macro's **own #define** | the directive's own line, read from the file's text | the index: a file opened a moment ago has a view and no summary yet |
| hover on a **predefined** macro (__STDC_VERSION__) | the standard's own table, then the toolchain's -dM output | a compiler: __STDC__, __STDC_HOSTED__, __STDC_VERSION__ and __cplusplus are the *standard's*, and their values follow from the language the file is read as |
| hover on a **keyword construct** (_Static_assert, _Generic, 	ypeof, _BitInt, _Alignas, _Atomic, _Noreturn, _Thread_local) | the vocabulary, keyed by spelling | a keyword token: these are contextual spellings the lexer calls identifiers |
| **go to definition** on a macro | the same file-offset query the hover uses | a declaration: a macro has no name fact, which is why the jump answered nothing at all |

**Two defects behind those rows, both measured through the real server:**

* the go-to-definition handler asked only the *declaration* query, so SUM(total, 1, 2, 3) answered 
ull while the
  hover on the same name worked — the two handlers asked different questions of the same name;
* the hover handler holds the file's **plain** view (the cheap one, so that hovering does not evict the rendering cache),
  and every macro query read written_text(), which is None for a plain view — so *every* macro question in the
  editor answered "no written text". FileView::the_written_text now returns the file's text from whichever kind of view
  it is handed.

**And one that is not about macros at all, found while measuring them:** a .c file with no compilation database was
read as **C++23**, because the editor's default [compile] standard is c++23 and it outranked the file's extension.
Measured: language_for answered amily=Some(Cpp) standards=[Cpp23] on a .c fixture, so every C-only construct was
either an error or a name. The extension now vetoes a standard of the *other* family for .c/.i files; a C++ file
handed -std=c11 is left alone, because that is a project saying something unusual rather than a default leaking.
## 6. Diagnostics

Thirty-one rules, each with a fixed severity; `[diagnostics].disable` turns one off by name. The rows marked **C++**
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
| `dangling-return` | Error | the address of a local returned as a pointer, or a pointer into a local |
| `dangling-field` | Error | a pointer member initialised from a by-value parameter or a temporary — **C++** (C has no constructors) |
| `use-after-free` | Error | a pointer read after `free`/`delete` released it, including a free written in another function |
| `ue-naming` | Hint | Unreal Engine's prefixes — **C++**, and only with `[unreal] enable` |
| `ue-generate-body` | Error | a `UCLASS`/`USTRUCT` without `GENERATED_BODY()` — **C++**, and only with `[unreal] enable` |
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

## 11. `...` is a token of the list, not a parameter node

```c
#define C23_SUM(count, ...) (c23_sum_impl)(count __VA_OPT__(, ) __VA_ARGS__)
static int c23_sum_impl(int count, ...) { return count; }     /* a definition */
int c23_va_opt(int count, ...) { return C23_SUM(count, 1, 2, 3) + count; }
```

`argument-count` said *"`c23_sum_impl` takes 1 argument(s) and this call passes more"* — the expansion passes four. A
**definition's** parameter list is read from the tree, and a `...` is a *token of the list*, not a parameter node:
`int count, ...` has one child (`int count`) and the ellipsis beside it, so a loop over the children never saw it. A
**prototype's** list reaches the same rule as the declaration's *text*, so the defect only appeared for functions
*defined* in the file being checked — which is why the first regression test (a prototype) passed while the corpus
failed.

Both shapes are pinned now: `a_variadic_macro_call_is_not_an_argument_count_finding` (prototype) and
`a_variadic_macro_whose_callee_is_a_definition_is_not_an_argument_count_finding` (definition, the corpus verbatim).

## 12. Completion and signature help, measured rather than changed

A handshake test asks, over stdio, for a function the file defines, a macro the file defines, and the signature of
`fprintf(` from `<stdio.h>`: all three answer. The corpus probe agrees (200 items at `C23_SUM(`, with `C23_SUM` and
`c23_sum_impl` among them), and `tests/c_project.rs` against the corpus reports `documentSymbol 30`,
`semanticTokens 339`, `inlayHint 2`, `diagnostic 0`.

The corpus probe grew a **completions** section for this reason: the member/type checklists answer *"what does the
engine know"*, and a reader's complaint is about *"what am I offered"* — two different questions that read the same
until one of them is measured separately.

## 13. The completion budget is spent fairly, not first-come

Reported at a specific position — `fprintf(` inside the last function of `functions.c`. Measured **through the real
server** at that exact line and column:

```text
completion (reported)   no   200 items, 10 ms   fprintf: false, isIncomplete true, first: ["code", "add", "average"]
```

Two hundred items, ten milliseconds, the analysis healthy — and the name the reader was typing absent. Every name of
every included header has the same score, so the alphabetical tiebreak decided which 200 of thousands survived
`ITEM_BUDGET`, and an alphabetical cut of five standard headers reaches only `<stdio.h>`'s `a`–`b`. The reader types
`fpr`, the **client** filters the 200 it already has, and the popup is empty — "completion never works here" was a
truncated answer, and a count does not show truncation.

`ordered` now draws **round-robin per declaring file** and re-sorts the chosen items by the same rank: the fair draw
decides *which* items survive, never their order. The same position now answers `fprintf: true`, and the probe's rows at
`fprintf(`, `fp`, `fpr`, `fprint` and `fprintf(stderr` all hold it.

The handshake test for a C reader's three questions includes the **empty-prefix** row with five standard headers in the
fixture, so the fixture overflows the budget by itself: a first-come cut fails it.

**On "sometimes slow":** the same probe prints `the project is quiet after 33.8 s` — the *initial* index of this corpus
(nine files plus the MinGW headers each inclusion pulls in). While it fills, an answer is a subset with
`isIncomplete: true`, which an editor shows as a list that grows and a popup that re-opens. The query itself is 10 ms at
this position.

## 14. The completion budget, in three parts

```text
the typed prefix   dropped when nothing matched it, BEFORE the budget — with a prefix the answer is exact and never
                   truncated: `fpr` answers 3 items, `fp` 10, `fprint` 2
the budget         1000 items (clangd's own default), so an unfiltered list of a real C file fits whole
the fair draw      only when 1000 is still not enough: round-robin per declaring file
```

Measured on the corpus: the **complete** visible set at `fprintf(` is 411 names and at `C23_SUM(` 481 — so the earlier
200-item budget was cutting a list that fits, and the cut was what made `fprintf` invisible. Both are answered in full in
**11 ms**, the same figure the truncated list took.

A limit is still needed *in principle*: a C++ file that includes the standard library sees **14 351** names, which was
4.5 MB of JSON per keystroke when the budget was added. The prefix filter is the part that was missing and matters most —
non-matching names were *ranked last* rather than dropped, so they spent the budget and a typed prefix could not rescue
the list it was typed into.

## 15. Two things a reader notices at one character

**The order changed.** With `fp` typed in `functions.c`:

```text
before   functions_plain, functions_pointers, fprintf, fputc
after    fprintf, fputc, fputchar, fputs
```

The tier (where the declaration is) was compared *before* how well the name answered what was typed, so a local that
matched `f`…`p` as word starts outranked the name whose first two letters those are. The match quality now leads and the
tier is the tiebreak — which is what `completion::matching` documented all along and what the ranking note contradicted.

**An answer that answers nothing is never final.** A client filters by the prefix and caches an empty result, so only
the next keystroke re-opens the question — the shape of *"输入 f 没有补全, 输入 fp 就有了"*. `CompletionSet::nothing_matched()`
asks the engine's own matcher whether any item could be what was typed, and the handler marks such an answer
`isIncomplete: true`, so the client re-asks instead of caching "nothing here".

Measured one-shot through the real server at the reported position: `427 items, 15 ms, fprintf: true`. The handshake
suite gained two rows — the first completion after `didOpen` must hold the same name as the second (one shot each, no
retry, because a retry hides the race), and `fp` must lead with `fprintf`.
