# Worklog — what was measured, what was wrong, what fixed it

A record rather than a map. [`c-support.md`](c-support.md) says what C is read as and which rules can fire;
[`cpp-support.md`](cpp-support.md) does the same for C++; this file says what a real program actually reported, which
of those reports were the analysis's mistakes, and what closed them. New entries go at the top.

Every number here was taken with a project's **own** build configuration (a compilation database when the project has
one, a synthesized one from its Makefile when it does not), and every entry names the commit that made the difference.
The protocol for asking one question at a time is [`inspect.md`](inspect.md).

---

## nlohmann/json 3.12.0 — the first **C++** library measured

A header-only library, so the measurement is not one project but three questions: the library's own code, its test
suite, and the shape of the framework the tests are written with. The tree is `E:\corpora\json` (3.12.0):
`include/nlohmann/**` (84 modular headers), `single_include/nlohmann/json.hpp` (the 25 000-line amalgamation), and
`tests/src/unit-*.cpp` (126 translation units using doctest).

The database is synthesized, because a header-only library has no build of its own:

```text
g++ -std=c++17 -I include -I tests/src -I tests/thirdparty/doctest -c tests/src/unit-<name>.cpp
```

| Question | Answer | Findings |
| --- | --- | --- |
| the library, through the **amalgamation** (`probe_amalgamation.cpp`: `#include <nlohmann/json.hpp>`, `-I single_include`, an object literal, `dump()`, `get<int>()`) | **Cooked, clean** | **0** |
| the **test suite**, one `cpp_inspect` process per TU (121 of 126 answered) | 19 of 121 TUs do not parse | **92 `syntax-error`** |
| 5 TUs the tool could not answer | "has no reading to diagnose" | — |

So the library reads clean; what does not is a **macro shape inside the test framework's use**: every failing TU's
first error is reported at its first `TEST_CASE("…")` line (`unit-conversions1.cpp:53`, `unit-binary_formats.cpp:27`,
`unit-bjdata.cpp:4480`, …), and the errors after it fall inside that case's body. A probe carrying the file's own
preamble (16 lines of includes and defines) plus `TEST_CASE("x") { CHECK(1 == 1); }` is **clean**, and so are nested
braced initialisers (`std::map<std::string, std::vector<int>> m = {{"a", {1, 2}}};`) and the exact
`json::object_t o = {{"object", json::object()}, {"array", {1, 2, 3, 4}}};`. The trigger is therefore inside the
bodies the tests write — **open**, with the file list and the repro command in `target/bench/json-sweep.json`. The
bisect so far, each probe a single `cpp_inspect diagnostics --cook` run on a TU that includes only what is named:

| probe | content | result |
| --- | --- | --- |
| the file's own preamble (lines 16-53) + `TEST_CASE("x") { CHECK(1 == 1); }` | | **fails** |
| … with the two `DOCTEST_CLANG_SUPPRESS_WARNING_*` lines removed | | **fails** |
| … with the `#if __has_include(<optional>)` block removed | | **fails** |
| … with `#define JSON_TESTS_PRIVATE` removed | | **fails** |
| doctest + `json.hpp` + `using` + all seven std headers + `TEST_CASE{…}` | | clean |
| the same, plus `JSON_TESTS_PRIVATE` (before *or* after `json.hpp`) | | clean |
| the same, plus the `__has_include` block | | clean |
| the same, plus the comment and **both suppress macros** | | **fails** |
| `#include "doctest_compatibility.h"` + both suppress macros + a function | | clean |

So the failure needs **doctest + nlohmann/json + the seven std headers + doctest's two pragma macros in one TU**: it is
a state interaction rather than one construct, and the next step is to shrink it by one include at a time from that
failing probe (the script is the one that produced the table: a Python loop over probe files with one CDB entry each).

### The root cause of the json failures: hedley's macros are not in the cook's table

Once `cpp_inspect rendering` existed, the failing unit could be read instead of guessed: `unit-conversions1.cpp` cooks
to **240 434** bytes and parses with exactly **eight** errors, all `expected ; after expression`, all at three
places — the bodies of `equal_general`, `less_general` and `equal_leaf`:

```text
… noexcept { JSON_HEDLEY_DIAGNOSTIC_PUSH JSON_HEDLEY_PRAGMA(GCC diagnostic ignored "-Wfloat-equal")const auto lhs_type = lhs .type(); …
```

`JSON_HEDLEY_PRAGMA(value)` is `_Pragma(#value)` (`include/nlohmann/thirdparty/hedley/hedley.hpp:742`), an operator
with **no `;`**, so a parser that reads it as a call expression reports the error against the *declaration after it* —
which is exactly what the eight errors are. Two separate defects then:

* **the cook leaves hedley's function-like macros unexpanded** — the rendering still spells `JSON_HEDLEY_PRAGMA(…)`,
  `JSON_HEDLEY_DIAGNOSTIC_PUSH` and `JSON_HEDLEY_UNLIKELY(…)` (61 occurrences) — so whatever the parser does, the
  stream it reads is not the compiler's. **And it is not specific to the failing tests**: a four-line translation unit
  whose whole content is `#include <nlohmann/json.hpp>`, one `JSON_HEDLEY_UNLIKELY(1)` and one
  `JSON_HEDLEY_PRAGMA(GCC diagnostic push)` cooks to **150 534** bytes with `JSON_HEDLEY_UNLIKELY(` **62** times,
  `JSON_HEDLEY_PRAGMA(` **4** times, and `__builtin_expect` / `_Pragma(` **zero**. So hedley's definitions never reach
  the table at all, while `__GNUC__` does (a fixture printing `gnuc_seen` proves the guards run). The next question is
  the one line that decides it: `hedley.hpp:17` is `#if !defined(JSON_HEDLEY_VERSION) || (JSON_HEDLEY_VERSION < 15)` —
  if the cook already holds `JSON_HEDLEY_VERSION` when it reaches the file, the whole of hedley is skipped, and the
  includer that gets there first is `detail/macro_scope.hpp:12`, ahead of `json.hpp:88`. That is where the next
  session starts.
* **the parser had no rule for a pragma operator between statements** — fixed here (`stats.rs`,
  `a_pragma_operator_between_statements_is_a_statement`), which is the *expanded* form hedley and doctest produce;
  `FORMAT_VERSION` 47 → 48 for the grammar change.

### `--cook` drained eight includes, not the closure — and hedley contributes no tokens at all

Two more facts, both from the `rendering` command:

* **Five test files had no reading, and the tool was the reason.** `--cook` asked for the reading and then called
  `catch_up`, which reads the file and **eight** of its includes — the slice a hover needs — while a cook needs every
  include the walk will enter, or `cooking_materials` refuses. `unit-json_view.cpp` answered "has no reading to
  diagnose" and yet `rendering` built its 206 680-byte text. The flag now drains until `needs_catching_up` is false,
  and the measurement answers **126 of 126** files (it was 121) with **95** `syntax-error` — the earlier 92 plus the
  three the five unread files were hiding.
* **`hedley.hpp` reaches the cook as nothing.** In the four-line unit's 150 534-byte rendering, `JSON_HEDLEY_UNLIKELY(`
  appears 62 times, and `JSON_HEDLEY_VERSION`, `JSON_HEDLEY_GNUC_VERSION`, `JSON_HEDLEY_VERSION_ENCODE` and the word
  `hedley` appear **zero** times — the header is either not found (no `include-not-found` finding mentions it, so it is
  found) or its whole body is skipped by its first line, `#if !defined(JSON_HEDLEY_VERSION) || (JSON_HEDLEY_VERSION < 15)`.
  That is the next thing to read: who defines `JSON_HEDLEY_VERSION` before `detail/macro_scope.hpp:12` reaches the
  file, and whether the walk marks a header read and drops its second, *first* contribution.

### The probe files were measured on a rendering of nothing — the bisect below is void


Trying to shrink the failing shape to a fixture, every probe created as a new `.cpp` under `tests/src` answered from a
**114-byte rendering** — the file's own tokens with the include closure collapsed to one `using` line — while
`unit-constructor1.cpp` answers from **254 299 bytes** and `unit-binary_formats.cpp` from 245 659. The probe files are
in the same database, with the same `-I`; the difference is the file itself, and a cold cache does not change it. So
every probe result recorded above ("clean", "fails") was a statement about a rendering with no doctest and no library in
it, which is why probes contradicted each other and why `TEST_CASE` was reported at one place and not another.

What survives: the **measurement** itself, because it reads real translation units — the 19 failing files cook to
200–250 kB renderings, so their 92 syntax errors are errors of parsing a fully cooked TU. What does not survive: the
bisect table. The next step is to bisect **inside a copy of a real failing TU** (rename `unit-conversions1.cpp`'s text
into a file that is already in the database, rather than adding new files to it), and to find out why a *new* file
cooks without its closure — the summary cache key does not include the compilation command, or the walk gives up on a
new path. `cpp_inspect rendering` (added here) is the command that shows it: `--limit 1` answers the size.

### Two tool fixes the measurement needed


* **`--cook` cooked nothing.** `Session::cook` answers `None` for a file the index has no summary of, and the flag did
  nothing for a file that was not also indexed: `diagnostics` then failed with "has no reading to diagnose" on every
  nlohmann test file. The flag now asks for the file's reading and drains it (`want_cooked_reading` + `catch_up`)
  before cooking.
* **`sweep` measured a different reading than a client's pull.** The sweep implied `--index`, and
  `Session::diagnostics` *prefers a stored cooked reading* over the one just cooked — which is the index's reading,
  built without the file's own include path in a header-only project: doctest's
  `CHECK_THROWS_WITH_AS(…, json::out_of_range&)` stayed unexpanded and the sweep reported **2 457** syntax errors over
  151 files where the per-file path reports **92 over 19**. The sweep now seeds the files (`--all-files`) without
  indexing them, and cooks each one the way `--cook` does. The number a `sweep` reports is now the number a pull
  reports.

## curl 8.x — the third C project, and the cleanest so far

Cloned at `E:\corpora\curl`, configured with its own CMake and MinGW's gcc — a **real** database, not a synthesized one:

```text
cmake -S . -B build -G Ninja -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
      -DCMAKE_C_COMPILER=…/w64devkit/bin/gcc.exe
      -DCURL_USE_OPENSSL=OFF -DCURL_USE_LIBPSL=OFF -DCURL_USE_LIBSSH2=OFF
      -DUSE_LIBIDN2=OFF -DCURL_ZLIB=OFF -DCURL_BROTLI=OFF -DCURL_ZSTD=OFF
      -DBUILD_TESTING=OFF -DBUILD_EXAMPLES=OFF
```

255 entries (`lib/` 210, `src/` the rest); `compile_commands.json` copied beside the project root, which is where the
session looks for it.

| Measurement | files | findings | errors |
| --- | --- | --- | --- |
| `lib/` | 152 | **68** | **0** |
| `src/` (the command-line tool) | 44 | **13** | **0** |

No `syntax-error`, no `undefined-name`, no type finding — in either half. All 81 findings are warnings, and each class is
one of the three the earlier projects named:

| Code | Count | What it is |
| --- | --- | --- |
| `missing-include` | 56 + 13 | "`Curl_easy` is declared in `curl_trc.h`, which this file does not include" — in a **header** (`lib/cf-h1-proxy.h`) that every translation unit includes *after* `curl_setup.h`. Plausible "include what you use", noisy for curl's header style. The attribution ("declared in `cf-https-connect.h`") also looks arbitrary for a name several files declare — the *first* declaring file the index happened to hold. |
| `include-not-found` | 10 | **Platform headers, not this platform**: `ConditionalMacros.h` (macOS), `<sys/socket.h>`/`<gskssl.h>`/`<qsoasync.h>` (OS/400's `setup-os400.h`), `<pwd.h>`/`<unixlib.h>`/`<openssl/evp.h>` (VMS's `setup-vms.h`). Those files are guarded out of a Windows build; the sweep scans them because they are `.h` in `lib/`. |
| `macro-parentheses` | 2 | `#define argv_item_t char *` and `#define curl_thread_t …` — macros that stand for a **type**. Parenthesising the replacement list would break the declaration, so this is a rule refinement (a body that is a type is not an expression), not a defect in curl. |

That is the third C project measured end to end, and the first with **no errors at all** on its first run — after
`f53b5bc6`, `db1bb5c6` and the two rule fixes above. It is also the strongest evidence so far for the claim the work
started from: what a C project reports is dominated by whether its build configuration is real, and by then by style
warnings, not by the analysis failing to read the code.

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
| … and a parenthesised function declarator (`db1bbcd3`) | 17 (0 syntax) | 181 | 902 |
| … and two more probes of the same kind (`f0e2c1d4`-era): a multi-declarator's type, the assignment idiom | **16 (0 syntax)** | 181 | 902 |

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

### 6. Two rules and one reader, from the same measurement — fixed

Three defects, all found by reading the sweep's output rather than by a report:

* **`return-type-mismatch` on `src/dict.c:1156`.** `unsigned long long integers[6], hash = 0;` followed by
  `return hash;` was reported as returning a value that does not convert — because the reader that answers "the type
  this name was declared with" took the declaration's **first** `InitDeclarator`, so `hash` inherited `integers[6]`'s
  array. `declarator_declaring` now picks the `InitDeclarator` **that holds the name** (`sema/types.rs`), and the same
  reader answers `unsigned long long` for `hash` and `int` for `a` in `int a = 0, b[6];`. Fixture
  `types::each_declarator_of_one_declaration_has_its_own_type`.
* **`assignment-in-condition` on redis's own idiom.** `if ((ln = listNext(&li)))` — the assignment is wrapped in its
  own parentheses, which is exactly how C says "this one is deliberate"; gcc's `-Wparentheses` does not warn for it
  either. The rule knew about `((x = f()) != 0)` (a comparison) but not about the bare doubled parenthesised
  assignment, and reported **13** of them. It is now silent for that spelling, and `if (x = f())` is still reported.
* **The `dangling-return` one is *not* fixed, and the attempt was reverted.** `src/t_hash.c:4192` returns
  `&dictExpireMeta->elemExpires` where `dictExpireMeta` is a local **pointer**: the referent is the object the pointer
  points at, not the local, so the finding looks wrong. `CppMemberExpr::is_arrow()` is the right question and the
  change was written — but the rule still reported the shape through the cooked reading, and a `codes_of` fixture did
  not reproduce the case at all (a local pointer to a struct, `&meta->expire`). A change that no test and no
  measurement shows is a change not made: both the change and its test were removed, and this stays an **open** item
  whose next step is to instrument `origin_of` on the cooked tree of `t_hash.c`.

After the first two: redis 8.x src, 222 files — `assignment-in-condition` 12 → **0**, `return-type-mismatch` 1 → **0**.
The probe that produced the earlier error counts is deleted (§7 of `inspect.md`), so the tally below is `sweep`'s: 222
files, and the Error-severity codes are `undefined-name` 23 and `dangling-return` 1.

### What is left, and whose defect it is

### 6. Two rules and one reader, from the same measurement — fixed

Three defects, all found by reading the sweep's output rather than by a report:

* **`return-type-mismatch` on `src/dict.c:1156`.** `unsigned long long integers[6], hash = 0;` followed by
  `return hash;` was reported as returning a value that does not convert — because the reader that answers "the type
  this name was declared with" took the declaration's **first** `InitDeclarator`, so `hash` inherited `integers[6]`'s
  array. `declarator_declaring` now picks the `InitDeclarator` **that holds the name** (`sema/types.rs`), and the same
  reader answers `unsigned long long` for `hash` and `int` for `a` in `int a = 0, b[6];`. Fixture
  `types::each_declarator_of_one_declaration_has_its_own_type`.
* **`assignment-in-condition` on redis's own idiom.** `if ((ln = listNext(&li)))` — the assignment is wrapped in its
  own parentheses, which is exactly how C says "this one is deliberate"; gcc's `-Wparentheses` does not warn for it
  either. The rule knew about `((x = f()) != 0)` (a comparison) but not about the bare doubled parenthesised
  assignment, and reported **13** of them. It is now silent for that spelling, and `if (x = f())` is still reported.
* **The `dangling-return` one is *not* fixed, and the attempt was reverted.** `src/t_hash.c:4192` returns
  `&dictExpireMeta->elemExpires` where `dictExpireMeta` is a local **pointer**: the referent is the object the pointer
  points at, not the local, so the finding looks wrong. `CppMemberExpr::is_arrow()` is the right question and the
  change was written — but the rule still reported the shape through the cooked reading, and a `codes_of` fixture did
  not reproduce the case at all (a local pointer to a struct, `&meta->expire`). A change that no test and no
  measurement shows is a change not made: both the change and its test were removed, and this stays an **open** item
  whose next step is to instrument `origin_of` on the cooked tree of `t_hash.c`.

After the first two: redis 8.x src, 222 files — `assignment-in-condition` 12 → **0**, `return-type-mismatch` 1 → **0**.
The probe that produced the earlier error counts is deleted (§7 of `inspect.md`), so the tally below is `sweep`'s: 222
files, and the Error-severity codes are `undefined-name` 23 and `dangling-return` 1.

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
