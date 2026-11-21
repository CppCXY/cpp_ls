# Worklog — what was measured, what was wrong, what fixed it

A record rather than a map. [`c-support.md`](c-support.md) says what C is read as and which rules can fire;
[`cpp-support.md`](cpp-support.md) does the same for C++; this file says what a real program actually reported, which
of those reports were the analysis's mistakes, and what closed them. New entries go at the top.

Every number here was taken with a project's **own** build configuration (a compilation database when the project has
one, a synthesized one from its Makefile when it does not), and every entry names the commit that made the difference.
The protocol for asking one question at a time is [`inspect.md`](inspect.md).

---

## redis 8.x — the second C project, and the first measured with a database

131 files (`src/*.c`), a database synthesized from `src/Makefile`'s own flags: `gcc -std=gnu11 -pedantic
-DREDIS_STATIC= -D_GNU_SOURCE -DUSE_JEMALLOC` and the seven `-I../deps/…` paths the Makefile composes. Measuring on
Windows with a MinGW toolchain is a *platform* choice, and it is why the remaining `include-not-found` findings are
about POSIX headers rather than about redis.

| Measurement | errors | warnings | hints |
| --- | --- | --- | --- |
| scan only, no database (§8.1 below in the old numbering) | 427 (4 syntax) | 573 | 975 |
| with the synthesized database | 38 (2 syntax) | 181 | 850 |
| … and C's string-literal rule (`e023c54e`) | 19 (2 syntax) | 181 | 829 |
| … and a parenthesised function declarator (`4b6c8f1a`-era) | **17 (0 syntax)** | 181 | 902 |

### 1. A macro parameter may be spelled with a keyword — `d0de833f`

`#define createBoolConfig(name, alias, flags, config_addr, default, is_valid, apply)` — `default` is a keyword, and
`parse_parameters` accepted only `Identifier`s. The parameter list was cut there, the rest of it became the macro's
**body**, and the macro became *object-like*: redis's whole `standardConfig static_configs[]` table stayed unexpanded
text. **952 of the file's 1002 syntax errors were `src/config.c` alone.** The preprocessor's grammar has no keywords
(C11 6.10.3), so a parameter is judged by the same rule the macro's own **name** already used (`could_be_a_macro_name`,
which exists for `#define true 1`). Syntax errors 501 → 4; errors 904 → 427.

Fixture: `preprocessor_against_the_compiler::a_parameter_spelled_with_a_keyword_expands_as_the_compiler_says`, compared
against `gcc -E`.

### 2. A header the client calls C is not judged by C++ rules — `a2676c79`

`cstyle-cast`, 73 findings, **all in `.h` files**: `(int)p` is a C-style cast in C++ and *the* cast in C. A header's
family is deliberately nobody's guess (it is included by whichever language includes it), so a `.h` took the
project-wide standard — `c++23` in an editor's defaults — and `is_cpp_only`'s rule "not told is not C" did the rest.
`didOpen`'s **`languageId`** is the statement nothing else makes; the server was discarding it. It is kept per open
document and used for a file the compilation database does not name. `cstyle-cast` 73 → 0.

Fixture: `cpp_ls/tests/a_header_language.rs` — the same header, once with `languageId: "cpp"` (finding present), once
with `"c"` (nothing), both through the real server.

### 3. C's string literal is `char[]`, not `const char[]` — `e023c54e`

Every one of twenty findings was one pair: a string literal meeting a `char *`. In C the literal's type has **no
`const`** (C11 6.4.5p6), so `char *p = "…";` is exact; the type layer spells a literal `const char*` in both languages
because a spelling is one answer for two. The language is known at the check layer and the expression is in hand there,
so the three rules that ask a conversion question consult `CheckContext::a_string_literal_in_a_char_pointer_position`.
Only a literal: a `const char *` variable dropped into a `char *` is a discarded qualifier in C too. 20 → 0, and the
C++ half is untouched.

Fixture: `assign_type_check::a_string_literal_in_a_char_pointer_position_is_c_only` (silent in C, reported in C++).

### 4. A calling convention is not the pointer — `e023c54e`

MinGW writes `char *__cdecl strchr(const char *, int)`; the tree is
`Declarator { Declarator { PointerType { Star, Identifier "__cdecl" } }, NameExpr "strchr", … }`. The return spelling
was assembled from the **text** before the name — `*__cdecl` — which the "purely operators" filter refused, so
`strchr` recorded `returns: char`. Every caller then read it as returning a `char`, and
`char *pch = strrchr(path, '/')` was reported as a conversion that does not exist. The operators are now read from the
**tree** (`*` per `PointerType`, `&` per `ReferenceType`), so a word beside the star cannot hide it; a parenthesised
return type (`int (*)(int b)`) still wins, and the text keeps the shapes the tree says nothing about.

Fixture: `declarations::a_function_records_what_it_returns_and_nothing_else_does`'s `conventional` case.

### 5. The two syntax errors: a macro standing for a variable — fixed

Both files failed at their first declaration after the includes, with `expected ), but get identifier`:

```
sentinel.c:26      extern char **environ;
server.c:2620      extern char **environ;
```

`extern char **environ;` parses on its own in either dialect, and the whole file read clean through `cpp_inspect`, so
the line was not the problem. The **rendering** was: `CPPLS_DUMP_RENDERING_DIR` showed the cooked stream starting with

```c
extern char **(*__p__environ());
```

which is MinGW's own `#define environ (*__p__environ())` expanding — correctly — inside redis's declaration. The shape
is a **parenthesised declarator whose inner declarator is a function**, and `gcc -std=gnu11 -fsyntax-only` accepts it.
Our parser did not: `a_parenthesised_declarator_with_a_name_follows` claims a group only when it is `( <op> name )`,
so this group was read as an *abstract* pointer-to-type, whose reading wants a parameter list after the `)` — there is
none, the declaration failed, and the damage landed on the line after the includes (which is what made it look like the
includer's fault).

Two changes, both in `types/declarators.rs`:

* the guard also claims `( <op> name (` — a group whose inner declarator is a function (`(*f())`), while `(*f)(int)`
  stays on the arm that already had it;
* `parse_parenthesised_declarator` reads that inner name's own parameter list before the group's `)`, so the suffix
  belongs to the function inside rather than to the pointer outside.

Measured: redis **19 → 17 errors, 2 → 0 syntax errors** (hints 829 → 902 — the two files now parse in full), and libuv
unchanged at **0**. The C++ shapes are untouched: `int (*fp)(int)`, `int (*maker(int a))(int b)`,
`void (*signal(int, void (*)(int)))(int)` and `typedef int (*binary_op)(int, int)` all parse, and the parser's
501 tests pass.

Fixture: `char **(*p__environ2(void));` and `char **(*f());` in `target/bench/environ2.c`'s shape — a parser test is
added beside `a_second_parameter_list_after_a_macro_type_is_one_declarator`.

### What is left, and whose defect it is

### What is left, and whose defect it is

| What | Count | Whose |
| --- | --- | --- |
| `include-not-found` | 92 | **The measurement's**: `<sys/socket.h>`, `<poll.h>`, `<sys/epoll.h>` are POSIX headers, and redis is a Unix program measured with a MinGW toolchain. On Linux these resolve. |
| `macro-parentheses` | 58 | redis's own style (`atomicvar.h`); the rule is a Warning and the bodies really are unparenthesised. |
| `missing-include` | 19 | Follows from the same absent headers. |
| `undefined-name` | 15 | `ae_epoll.c`/`ae_kqueue.c`/`ae_evport.c` are **platform files** this build would not compile; my database listed every `src/*.c`. Correct findings about the wrong platform. |
| `assignment-in-condition` | 12 | gcc's `-Wparentheses` says the same. Not triaged line by line. |
| `syntax-error` | 0 | Fixed above: a macro standing for a variable made a parenthesised function declarator out of `extern char **environ;`. |
| `return-type-mismatch` | 1 | `src/dict.c:1156` — **open, diagnosed**: `unsigned long long integers[6], hash = 0;` followed by `return hash;`. The **fact** for `hash` is right (`unsigned long long`, `cpp_inspect facts`), while the **expression type** of `hash` is `unsigned long long[6]` (`cpp_inspect type --needle "return hash;" --shift 8`) — the second declarator inheriting the first one's array suffix somewhere between the scope tree and the type engine. Reproduced minimally in four lines; the search is where a name's type is answered, not where facts are built. |
| `dangling-return` | 1 | `src/t_hash.c:4194` — a pointer into a local struct member. Plausible; not triaged. |

---

## How to reproduce any number here

The probes these numbers were taken with are **gone**; the instrument is `cpp_inspect` (protocol:
[`inspect.md`](inspect.md)). A project's tally, by code:

```text
cpp_inspect --project E:\corpora\redis --file src/dict.c sweep
cpp_inspect --project E:\corpora\libuv --file src/win/core.c sweep
```

A single question — the one that found most of the defects above:

```text
cpp_inspect --project E:\corpora\redis --file src/dict.c type --needle "return hash;" --shift 8
cpp_inspect --project E:\corpora\libuv --file include/uv.h   facts --name uv_translate_sys_error
```

`--code` and `--including` narrow a sweep; `batch` asks many questions in one process, which is what a whole-project
investigation actually does. A project with no `compile_commands.json` is scanned, so the first number for a new
project is always "what the analysis says when nobody has told it anything" — the redis table above is what the second
number looks like once a database from the project's own Makefile is supplied.

## libuv 1.53.1 — the first C project


The first C project measured end to end, because "usable for C" is a claim about a program rather than about a grammar.
libuv is the right first subject: C11, `typedef struct` and macros all the way down, and its Windows build is one
compile database over 40 files (`src/*.c`, `src/win/*.c`):

```text
CMAKE_EXPORT_COMPILE_COMMANDS=ON with MinGW's gcc,
-std=gnu11 -Iinclude -Isrc -DBUILDING_UV_SHARED=1 -DWIN32_LEAN_AND_MEAN -D_WIN32_WINNT=0x0A00
80 database entries, 40 distinct files (the shared and static libraries each list every file)
```

| Measurement | errors | of which `undefined-name` |
| --- | --- | --- |
| first run | 12 | 11 |
| `db0e3f18` — a `typeof`/`decltype` specifier declares no written type | 11 | 11 |
| `f53b5bc6` — an X-macro enum is one enumeration, not rubble | 3 | 3 |
| `2ae8a432` — a parameter list after another is one more suffix | **0** | 0 |

Final state: **0 errors, 0 warnings, 0 syntax errors, 97 hints** (85 `braces-around-statements`, 12
`else-after-return` — both Hint by the registry's own table, and libuv earns them by writing `if (!handle) return
UV_EINVAL;`). Hover 466/526 (89 %), definition 403/526 (77 %) on a 92-file run of the same project.

### 1. `assign-type-check` on a type the compiler deduces — `db0e3f18`

`src/uv-common.c:576` reported `` `__atomic_exchange_val` is initialised with a value that does not convert to `void` ``.
The name is GCC's: `atomic_exchange_explicit` expands a statement expression whose second line is
`__typeof__ ((void)0, *__atomic_exchange_ptr) v = (VAL);`. The **type reader** spelled that type `void` — the
`BuiltinType` inside the `typeof`'s own argument won the walk — so the declaration was judged against a type nobody
wrote. Probed on the reader itself: `__typeof__ ((void)0, *p)` → `Builtin { spelling: "void" }`, `__typeof__(p)` →
`Builtin { spelling: "__typeof__(p)" }`.

The fix is a question about a **keyword kind**, not a spelling: the lexer already maps `decltype`, `typeof`,
`typeof_unqual`, `__typeof__` and `__typeof` to `DecltypeKeyword`, and `read_specifiers_named` answers "no written type"
for that kind. **`auto` deliberately keeps its spelling** — it is the marker the deduction path reads, and narrowing the
rule until the deduction tests passed is what the rule is a record of.

### 2. `undefined-name` on a name a header declares — `f53b5bc6`

Eleven findings, all about `uv_translate_sys_error` and its neighbours, which `include/uv.h` declares at file scope. The
header's summary held **21 declarations of 563**. The parser was not the cause: `cpp_dump` reported 0 errors in both
dialects. The cause was the enum every C library writes:

```c
typedef enum {
#define XX(code, _) UV_ ## code = UV__ ## code,
  UV_ERRNO_MAP(XX)
#undef XX
  UV_ERRNO_MAX = UV__EOF - 1
} uv_errno_t;
```

Two rules were missing, and both are the X-macro spelling: **an enumerator whose name takes an argument list** (one
enumerator *here*, many after expansion) and **no comma between that invocation and the next enumerator** (the commas
live in the macro's body, not in this file). Summary 21 → **788** declarations; libuv 11 → 3 errors.

### 3. `undefined-name` again, one construct along — `2ae8a432`

`WSAGetLastError` is declared in MinGW's `winsock2.h:1049`, and that header's summary held **206 of 313** declarations:
the walk stopped at line 899,

```c
typedef unsigned __LONG32 (WSAAPI *LPFN_INET_ADDR)(const char *cp);
```

which is `typedef unsigned long (__stdcall *F)(const char *)` before the macros are expanded. Read **unpreprocessed**,
`unsigned __LONG32` is a builtin and a *name*, so the first `(...)` became that name's parameter list and the second was
refused with `expected a parameter list or an initializer` — and the refusal rewound the declaration, so the recovery
ate the declarations after it. A `(` **straight after the declarator's name** still fails (that is `Widget w(g())`, and
the expression grammar must have it); a `(` **after a parameter list** is one more suffix. `winsock2.h`'s summary
206 → **809**; libuv 3 → 0.

### 4. The cache had to be told — `2ae8a432`

A stored summary is keyed by `READING_FINGERPRINT` — a hash of `cpp_parser::VERSION`, the crate version,
`FORMAT_VERSION` and the codec version — none of which move for a **grammar edit in a working tree**, so the first
re-measurement after the fix still read the old 206. That is what `FORMAT_VERSION` is for; it has moved six times since
(`44 → 47`), each with the reason written beside it.

---

## The C corpus before redis

The measurements that came before a named project, and what each one changed: the `neovim` C files (the corpus the C
work started from), Lua 5.5.0's `src/`, and the `test_c` fixtures in `crates/cpp_ls/tests/c_project.rs` — all recorded
where they were taken, in the commits between `557f9553` and `db0e3f18`. They are not repeated here because their
numbers were about individual rules rather than about a program's whole report; the two projects above are the ones a
claim about C usability rests on.
