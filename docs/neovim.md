# neovim, read the way an editor reads it

A census of `E:\neovim` (517 files under `src`, 218 of them `.c`), asked through the same session the server uses.
The probe is `crates/cpp_code_analysis/tests/neovim_probe.rs`:

```text
cargo test --release -p cpp_code_analysis --test neovim_probe -- --ignored --nocapture
```

`CPPLS_NEOVIM` names the checkout. The numbers below are one run of that probe, against a checkout whose **build
tree is built** — nothing is estimated.

```text
65 file(s) diagnosed (every eighth file), 3 with parse errors, 0 without a reading, 0 with unplaced errors, in 277 s
parse errors by message (top 3)
      9  expected `;` after expression  e.g. src/nvim/grid.h:20
      2  expected primary expression    e.g. src/nvim/grid.h:20
      1  expected `;`                   e.g. src/coverity-model.c:140
check findings by code
      9  UndefinedName     6  AssignTypeCheck   5  MacroParentheses   4  ElseAfterReturn
      3  BracesAroundStatements   3  Unused   2  IncludeNotFound   2  MissingInclude   2  Redefinition
names: 58 598 identifier(s), 53 920 classified (92.0%)
definition: 1 805 asked, 1 336 resolved (74.0%), 6 ambiguous, 463 unknown
type_at: 1 805 asked, 921 known (51.0%)
uv_* names: 15 asked, 14 resolved
```

**Why a sample.** With the dependency tree built, a file's include closure is the real one, and a **cold** reading of
one file costs ~30 s: every closure summary is keyed by the environment in force, and each root file has its own, so
the summaries are shared by nothing. The whole checkout is therefore a two-hour run the first time and ~8 minutes on
a warm cache. `CPPLS_NEOVIM_EVERY=8` asks every eighth file instead — 65 files, every directory of `src` — and
`CPPLS_NEOVIM_LIMIT` still caps the count. The run above is with `CPPLS_NEOVIM_CLOSURE=1`: every file is read the way
the editor reads one, including its direct includes.

The number to compare against is the first census of the same checkout, taken on another machine before the
dependency tree existed. It read all 517 files, so the rates — not the counts — are what line up:

```text
                                    first census (517 files)   this sample (65 files)
files with a parse error                28   (5.4%)               3   (4.6%)
parse errors                            59                       12
`new` lines unparsed                    13                        0
expected primary expression             18                        2
include-not-found                      177                        2
undefined-name                         258                        9
uv_* names resolved                     —                        14 of 15
```

The three files that still fail are not grammar defects, and are described in §5 and §6. The count moves between
runs — 2 or 3 files, with `grid.h` alone accounting for 5 to 12 errors — because which translation unit answers for
a header is not stable yet (§6); the resolution rates are what the fixes moved.

## 1. The build tree, built

Everything libuv, Lua and neovim's own code generator produce was missing or empty on the first checkout, and the
177 include misses plus most of the ~200 undefined names were a property of the machine rather than of the files.
It is built now, with the project's own tools:

```powershell
# the dependencies: libuv, LuaJIT, unibilium, utf8proc, tree-sitter, luv
cmake -S cmake.deps -B .deps -G "MinGW Makefiles" -DCMAKE_BUILD_TYPE=Release `
      -DCMAKE_C_COMPILER="C:/Users/zc/Desktop/mingw64/bin/gcc.exe"
$env:CMAKE_TLS_VERIFY = '0'      # this machine's proxy fails schannel's revocation check
cmake --build .deps --parallel 8

# the build itself, for the compile database and every generated header
cmake -S . -B build -G "MinGW Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_EXPORT_COMPILE_COMMANDS=ON `
      -DICONV_INCLUDE_DIR="C:/Users/zc/Desktop/mingw64/x86_64-w64-mingw32/include" `
      -DICONV_LIBRARY="C:/Users/zc/Desktop/mingw64/x86_64-w64-mingw32/lib/libiconv.a" `
      -DENABLE_LIBINTL=OFF
cmake --build build --target generated-sources
```

The two `-D` flags for iconv and `ENABLE_LIBINTL=OFF` are this machine's, not the project's: MinGW supplies
`iconv.h` and `libiconv.a` but no `libintl`, and the main `CMakeLists.txt` asks for both (`src/nvim/CMakeLists.txt:31`
and `:42`). Nothing else was configured by hand.

Three results matter to a reader of these sources:

```text
build/compile_commands.json           442 entries, gcc, -std=gnu99, an @includes_C.rsp per target
.deps/usr/include                     uv.h, luajit-2.1/lua.h, unibilium.h, utf8proc.h, tree_sitter/api.h
build/src/nvim/auto + build/include   212 + 465 generated headers
```

`.cppls.toml` therefore no longer writes `[compile] args`. A hand-written command line is a *second* description of
the build, and the one that was there (`-std=gnu11 -Isrc -Ibuild/cppls/generated`) had no dependency headers and no
generated inline headers — which is exactly where the 177 misses and the `xfree` findings came from: `memory.h`
resolves and includes `"memory.h.generated.h"`, and that file was two bytes long.

What the build changed, measured: **include-not-found 177 → 0**; `uv_*` names resolve (2 of 2 in the sample);
`xfree`, `Visual`, `msg_grid` and `verbose_enter` are declared, because the inline headers that declare them exist.

The cost is the other side of the same fact, and it is worth recording because it changes how a census is run: a
file's include closure is now the real one — `uv.h` pulls in `windows.h` on this machine — so a **cold**
`session.diagnostics` of one file takes ~38 s (the first read of its closure, ~200 files) and a warm one 0.3 s.
The probe therefore indexes the project **once** and asks each file for its reading; asking `index_everything`
after every file re-walked the whole project 517 times.

## 2. A C++ keyword used as a C name

*Fixed.* C reserves none of `new`, `delete`, `this`, `operator`, `template` and the alternative spellings
(`compl`, `not`, `and`, …); C++ reserves all of them, and the parser has one keyword table for both languages. So a
C file that uses one of those spellings as a name could not be read. What the project writes:

```c
src/nvim/autocmd.c:1343       Object new = BOOLEAN_OBJ(new_val);          // `new` is the object
src/nvim/marktree.c:2141      MTPos new = { new_row, new_col };
src/nvim/tui/termkey/driver-ti.c:197   struct trie_node_arr *new = …;      // and `new->arr[…]` at 200
src/nvim/marktree.c:392       void marktree_intersect_pair(…, bool delete)
src/nvim/insexpand.c:4116     compl = compl->cp_next;                     // `compl` is "completion" there
```

The gate is in `CppLexer::name_to_kind` (`a_word_c_does_not_reserve`) and in `the_alternative_operator`; the
language reaches the buffer view (`FileView::parse_with_cuda` takes it, fed from `Session::config_for_file`) and
the unit reading (`index_unit_rendering` passes `language_for(root)`). The fixture measurements:

```text
int new = 0;                                  C: no error      C++: error (the keyword)
void f(int delete) { }                        C: no error
int compl = 0; … compl = compl + 1;           C: no error
struct T { int x; }; struct T *new = 0; …     C: no error      `new->x` is a member access
```

On the machine the first census was taken on, this took 28 files with parse errors to 17 and 59 parse errors to 26;
13 `new` lines stopped being unparsable. Tests: `cpp_parser/tests/lexer.rs::a_word_cpp_reserves_is_a_name_in_c`,
`cpp_parser/tests/c_language.rs`, and `cpp_code_analysis/tests/checks.rs::a_c_file_may_name_things_cpp_reserves`
(the last one through `Session::diagnostics`, so the language plumbing is what is under test).

## 3. A parenthesised declarator is a declarator

*Fixed.* The C grammar's `( declarator )`, which neovim writes eight times at file scope:

```c
src/nvim/cmdhist.c:35          static histentry_T *(history[HIST_COUNT]) = { NULL, … };
src/nvim/mapping.c:69          static mapblock_T *(maphash[MAX_MAPHASH]) = { 0 };
src/nvim/option.c:210          static char *(names[3]) = { "TMPDIR", … };
src/nvim/syntax.c:169          static char *(spo_name_tab[SPO_COUNT]) = …
src/nvim/version.c:4109        static char *(lines[]) = { …
src/nvim/normal.c:4901         static const char *(ar[]) = …
src/nvim/ex_docmd.c:4247       static char *(p_bad_values[]) = …
src/nvim/highlight_group.c:76  static char *(hl_name_table[]) = …
```

Every one was unreadable. The group was read as a *type-id* — a function returning `char *` that names nothing —
so the `=` had no owner and the file reported ``expected `;` `` on the line that declares the table. The same gap
in a parameter list is `src/lpeg/lptree.c:1308`:

```c
static void createcat (lua_State *L, const char *catname, int (catf) (int))
```

`int (catf) (int)` is one parameter whose name is `catf`; it was read as two type-ids, so the whole definition —
body included — became an expression statement.

Three readings were wrong, and they needed three separate answers:

* `parse_init_declarator`'s parenthesised branch was entered only for the **function** spelling
  (`at_an_operator_led_parenthesized_declarator` requires a `(` after the `)`). `at_a_parenthesised_declarator_of_a_variable`
  adds the variable spelling — operator-led, a name with its array bounds in the group, followed by an initializer,
  `,`, `;`, `[` or a parameter list — and it asks `the_declaration_has_a_type`, which is what keeps `a * (b);` a
  product and leaves the expression grammar what belongs to it.
* `a_parenthesised_declarator_with_a_name_follows` did not know `(name)(`, the spelling a callback parameter is
  written with.
* `CppDeclarator::get_name` stopped at the first level, so even shapes the parser *did* read — `int (*f)(int);` —
  answered `None`, which `sema::scopes::is_unnamed_declaration` reads as "a call statement, not a declaration". It
  now walks into the nested declarator; `CppDeclaration::get_name` no longer lets an **elaborated** specifier
  (`struct S`) name the variable declared beside it; and `declarator_starts_with_a_type_keyword` sees through the
  specifiers that say nothing about the type (`static`, `const`, …), which was the whole difference between
  `static int *(…)` (the declaration ended at the `*`) and `int *(…)` (read, if wrongly).

Measured file by file, `inspect diagnostics --cook` on each of the eight:

```text
cmdhist.c:35          expected `;` at the `(`    clean — names `history`
mapping.c:69          expected `;` at the `(`    clean
option.c:210          expected `;`               clean
syntax.c:169          expected `;`               clean
version.c:4109        expected `;`               clean
normal.c:4901         expected `;`               clean
ex_docmd.c:4247       expected `;`               clean
highlight_group.c:76  expected `;`               clean … but the file's first error is now :72, a macro type (§6)
```

`histentry_T` and `mapblock_T` are typedefs in headers, so the parser's own type table cannot answer for them; the
`=` after the group is what decides, because the left operand of an assignment must be a modifiable lvalue and a
product is not one. The rule asks for a known type only for the followers that could continue an expression (`;`,
`,`, `[`, `(`), and it requires that something stand in type position at all — `*(p) = q;` is an assignment through
a pointer and keeps its reading.

Measured with `inspect` on the minimal shapes: `static char *(lines[]) = { 0 };` had one error at the `(` and
declared nothing; it parses and names `lines`. `void f(int (catf)(int)) { }` was an expression statement; it is a
declaration whose parameter is `catf`. Tests: `cpp_parser/tests/c_language.rs::a_parenthesised_declarator_names_what_it_holds`,
`::a_parenthesised_parameter_names_what_it_holds`, `::a_product_is_not_a_parenthesised_declarator`.

## 4. A prefix operator after a cast's parentheses

*Fixed.* `src/mpack/mpack_core.c:280`:

```c
byte_idx = (mpack_uint32_t)--remaining;
```

`(T)--x` is a cast of a prefix-decremented value, and it was read as a parenthesised expression: the `--` had no
left operand and the statement reported ``expected `;` after expression`` at the identifier behind it — the file's
only parse error.

The rule that decides a C-style cast without a type table is "an operand after the `)`", and its table leaves
`++`/`--` out for being "a binary operator as well as a prefix". That is not true of them: they are only ever
prefix or postfix. The token *after* one says which, and a prefix operator means the parentheses held a type,
because a postfix operator ends an expression and cannot begin an operand:

```text
(mpack_uint32_t)--remaining    a cast of `--remaining`
(a)--                          postfix: nothing follows
(a)-- - b                      postfix, then a subtraction — the token after `--` is `-`
```

Test: `cpp_parser/tests/operators.rs::a_prefix_operator_after_the_parentheses_is_the_casts_operand` (four prefix
spellings read as casts, four postfix ones keep their reading).

## 5. The two files that looked like a conditional-macro bug — not a parser defect

`src/nvim/os/env.c:323` and `src/nvim/os/pty_proc_unix.c:302` both sit under `#ifdef HAVE__NSGETENVIRON`'s
`#else`, whose `#if` arm writes `# define environ (*_NSGetEnviron())`. The reading applied that macro to the live
arm, or so it seemed. Measured, the macro is real and it comes from somewhere else:

```text
mingw64/x86_64-w64-mingw32/include/stdlib.h:709   #define environ _environ
                                                  (and `(*__MINGW_IMP_SYMBOL(_environ))` on the CRT arm)
```

`pty_proc_unix.c` is a **Unix-only file**: it is not in the compile database (the Windows build compiles
`pty_proc_os.c` and `pty_proc_win.c`), so the session falls back to the host toolchain — gcc on this machine — and
MinGW's `stdlib.h` defines `environ`. The `#define` inside the untaken `HAVE__NSGETENVIRON` arm is dropped
correctly: the file's own reading declares `extern char **environ;` and the tree is clean.

So this is a *configuration* fact, not a grammar one: a file that belongs to another platform is read with this
platform's headers. `env.c` — the other file — parses clean.

## 6. What is left

One reading is still wrong on this checkout, and it is not a grammar rule that is missing a case.

**A header is read in another translation unit's context — the *choice* is fixed, the *source* is not.** `src/nvim/grid.h:20` is

```c
EXTERN ScreenGrid default_grid INIT( = SCREEN_GRID_INIT);
```

where `EXTERN` and `INIT` come from `src/nvim/macros_defs.h`:

```c
#ifndef EXTERN
# define EXTERN extern
# define INIT(...)
#else
# ifndef INIT
#  define INIT(...) __VA_ARGS__
# endif
#endif
```

`src/nvim/main.c:3` defines `EXTERN` **for its own translation unit** — `# define EXTERN __declspec(dllexport)` on
Windows. Because the census indexes `main.c` as a project file, `EXTERN` is in the environment of *every* file, so
`grid.h` takes the `#else` arm and reads `__declspec(dllexport) ScreenGrid default_grid …`, which the parser refuses.
Measured both ways: `inspect diagnostics --cook --index` on `grid.h` alone answers **0 errors**; the same file in the
517-file session answered 2 at line 20. This is the "two readings of one file disagree" the first census recorded
for `drawline.c`. **Half of it is fixed**: `held_units` is a hash map, and `find` took *any* unit that contained the
header, so the answer changed with the map's layout; the choice is now the **lowest root path** among the units
held, and `session::tests::a_header_is_read_in_the_same_translation_unit_every_time` pins it (it failed on the
previous selection with the *other* root's macro).

**Fixed (2026-11-13)**: the choice now comes from the **index**, not from what the session has cooked.
`Session::the_unit_that_reads` walks the include graph's reverse edges (`includers_of`) breadth-first, shallowest
first and smallest path within a level, and stops at a file that can be the root of its own program — one with a
compile command, or one nothing includes. A unit the index does not name **never answers**: when the named one is
not held, the environment is composed from the index, which is the same reading the second pass builds a summary
from. The old rule ("the smallest root among the held units") made the answer a function of the session's history,
which is what two runs of one binary disagreed about.

Measured on the warm 52-file census: **165 s against 166 s** before the change — the choice costs nothing — with
0 files failing to parse and the same usability rates (`definition` 96.8%, `type_at` 97.6%, `uv_*` 5/5).

**What this did *not* fix, and where the next lever is.** The four `undefined-name` findings the census reports
are in `src/nvim/main.c:182` (`server_teardown`), `:186` (`signal_teardown`), `api/deprecated.c:583`
(`get_globvar_dict`) and `mbyte.c:3012` (`tv_list_alloc`). Those files are **roots**, read through their own walk
(or through the composed arm when their unit is not held) — this rule is about headers, so it cannot move them.
The same three files answer 0 findings through `inspect diagnostics --cook --index`, which is the other arm: a
root's cook can disagree with the tool's single-file cook about the environment (`EXTERN` is defined by a `.c`
file *for its own translation unit*, and which environment carries it is the same question one level up). That
is the next change.
**Why the walk is not simply replaced by the index's composition.** Composing every header's environment *is*
deterministic, and it was measured with the session's own test: a header's reading takes `value_b` from the
**includer's own text** (`#define KIND int value_b;` in `b.cpp`), and a composed environment never holds a macro a
`.c` file defines for itself, because composition walks *includes* and the index records headers' macros. With
composition only, that test finds **nothing** where the walk finds `value_b`. So an accurate header reading needs a
real translation unit, and *which one is available* is a property of the session — the next change has to take the
unit from the index and pay for its walk where it matters, rather than take whatever the memory holds.

**A summary is a shape-only reading, so a declaration a macro writes is not in the index.** Measured on the
dependency the build just added:

```text
inspect summary ... /uv.h    4 declaration(s), 61 macro fact(s)
inspect facts   ... /uv.h    uv__queue, next, prev, UV_ERRNO_MAP — and nothing after line 193
```

`uv.h` declares about 150 functions as `UV_EXTERN int uv_run(uv_loop_t *, uv_run_mode);` — neovim's use of libuv
is almost entirely those. The **summary** parses the file's own text with its `#define`s left as directives, so the
declaration reads `UV_EXTERN int uv_run(…)`: the type is a name the parser cannot place, and only the macros are
recorded.

**The cooked reading is supposed to answer for that — and it was rendering nothing.** The census first ran without
the editor's closure cook (`CPPLS_NEOVIM_CLOSURE=1`, `Session::want_the_closure_cooked`) — one file at a time, so no
header was ever cooked — and then with it, before the fix below and after it:

```text
                                one file at a time   closure cook, before   closure cook, after
uv_* names asked/resolved            15 / 2               15 / 2                 15 / 14
definition resolved                  73.0%                73.0%                  74.0%
type_at                              50.6%                50.6%                  51.0%
```

Measured on one real file — `src/nvim/event/loop.c`, which includes `uv.h` and calls `uv_run`:

```text
uv.h summary              4 declarations
uv.h cooked reading       0 declarations        <- filed as "this file has been read"
uv_run                    NotDeclaredHere
... after one more cook of uv.h alone:
uv.h cooked reading       2168 declarations
uv_* names                4 asked, 4 resolved    <- uv_run is there
```

The trace under `CPPLS_TRACE_COOKING` named it in one line: `render_cook: uv.h text 73771 tokens 13263 rendered 0
(composed)` — 74 KB of libuv's API in, **nothing** out. The **composed environment** (what a header is rendered with
when no program has read it) was built from every unconditional macro fact in the closure, *the file itself
included*, and a header's own guard is `#ifndef X / #define X` at the top, recorded as `Unconditional` on purpose
(`deguard_the_files_own_guard` — the file is read once per translation unit). So `X` was in force before the first
line, `#ifndef X` was false, and the guarded body was inactive. Every guarded header was affected: MinGW's
`stdlib.h` and `math.h` came out empty too.

**Fixed**: the file being rendered contributes nothing to its own starting environment — its `#define`s are what it
says, and the renderer reads them itself, in order, from offset 0. The same contract `MacroView::environment_at`
already states for a timeline. Measured after the fix, on the same file:

```text
uv.h cooked reading    1912 declarations
uv_* names             4 asked, 4 resolved
loop.c                 0 parse errors
```

Test: `session::tests::a_headers_own_guard_does_not_empty_its_rendering` — a two-line guarded header cooks to 0
declarations on the previous rule, and to its `Widget` after it.

**`Boolean`, `Dict(…)` and `Map(…)` — measured, and not a defect.** These were the last three files with a parse
error, and they are clean once the file is read the way an editor reads it:

```text
inspect diagnostics --cook            3 errors   (72:42, 920:1, 1710:11)   <- the tool alone, project not indexed
inspect diagnostics --cook --index    0 errors
```

The tool's `--index` is the difference, and it is not a subtlety of the grammar: without it the project is never
indexed, so the cook's environment holds no macro from any header, and `Map(cstr_t, int)` — `map_defs.h:43`,
`#define Map(T, U) Map_##T##_##U` — stays text. The server's pump indexes the closure before it cooks, so the editor
sees the declaration; `mapping.c:2774` (`Dict(keymap) *opts`, `#define Dict(name) KeyDict_##name`) is clean the same
way.

`src/coverity-model.c:139` is `return __coverity_alloc_nosize__()` with no semicolon. Invalid C, not ours.

`src/nvim/map_defs.h:52` — the first error of the old census, with the typedef `MTDamagePair` from
`src/nvim/types_defs.h:85` not seen as a type — parses clean now, with no other change to that file.

## 7. What is noise rather than a defect

`BracesAroundStatements`, `ElseAfterReturn` and `MacroParentheses` are style rules, and they fire on a C codebase
exactly as their clang-tidy namesakes do. They are not wrong — but on a project that has not adopted them they are
most of what a reader sees, which is the reason `[diagnostics] disable` exists.

## 8. Re-measured after the declarator and guard rounds

One run, one binary (2026-11-13), against the same checkout with the build tree built. **Every tenth file**
(`CPPLS_NEOVIM_EVERY=10`, 52 files of 517, 21 of them `.c`) with the editor's closure cook
(`CPPLS_NEOVIM_CLOSURE=1`) — the full checkout is a ~50-minute run by construction (each file is an open with its own
environment, measured at 4.7 s per file), so the census below is a *rate* with the sample named rather than a count.

```text
52 file(s) of 517 withheld; 21 are C          244.9 s      (4.7 s/file)
files with parse errors                        0
files without a reading                        0
files with unplaced errors                     0
`new` as a name                                0 site(s), 0 on a line that does not parse
include-not-found                              4   <execinfo.h> <langinfo.h>
                                                   <sanitizer/asan_interface.h> <sanitizer/ubsan_interface.h>
undefined-name                                 1   (Search)
names                                          43 200 identifiers, 35 307 classified (81.7%)
definition                                     1 495 asked, 1 094 resolved (73.2%), 17 ambiguous, 384 unknown
type_at                                        1 495 asked, 723 known (48.4%)
uv_* names                                     5 asked, 4 resolved
```

The four include misses are headers MinGW does not ship (glibc/sanitizer), not a search-path defect, and `Search` is
one name. The findings that remain are the style rules of §7 (BracesAroundStatements 15, MacroParentheses 5,
ElseAfterReturn 2, and one each of MissingInclude/MissingReturn/Redefinition).

One C file that exercises the dependency, `src/nvim/event/loop.c` (includes `uv.h`, calls `uv_run`), read the way an
editor reads it:

```text
parse errors 0 | names 354 identifiers, 339 classified (95.8%)
definition 26/30 resolved (86.7%) | type_at 18/30 (60.0%) | uv_* 4 asked, 4 resolved
```

The two rates are the honest gap that is left: **type_at is under half**, and the 384 unresolved definitions are the
same population — a name whose type is written by a macro or by a header that only a program read resolves. That is
the next round, and it is why the provenance of a name matters more than its count.

For comparison, the census that opened this document (before the keyword, declarator and guard fixes, and without the
dependency tree) had **17 files failing to parse, 26 parse errors, 283 undefined names and 175 include misses** on the
full 517.

**The denominators were wrong, and an enum bug was behind some of the misses.** The first version of this
section asked `definition` about every sampled token and `type_at` about every sampled token. Both counted names
the analysis does not answer for by design — `#define`'s own words (`define`, `endif`, `defined` are identifiers
lexically but not names), macros, type names, and functions whose *type* is a function type the hover answers. The
probe now asks `definition` only about **classified** names (`get-name 96.8%`), and `type_at` only about
**value-like** classified names — variables, parameters, fields, enumerators:

```text
definition      1 092 asked, 1 057 resolved (96.8%), 7 ambiguous, 28 unknown
type_at           829 asked,   536 known    (64.7%)
names          43 200 identifiers, 35 310 classified (81.7%)
```

And one real defect was found while reading the misses: **an anonymous enumeration was named after the first
identifier in its body.** The name is written between the keyword and the body, but the reader took the first
`NameExpr` in the subtree — which for `enum { MODE_NORMAL = 0x01, … MODE_REPLACE = REPLACE_FLAG | MODE_INSERT }` is
the `REPLACE_FLAG` of an enumerator's initializer. Measured on `src/nvim/state_defs.h` before the fix:

```text
facts state_defs.h --name MODE_INSERT      25:3  MODE_INSERT  variable in REPLACE_FLAG
hover mouse.c:894:18 (use of MODE_INSERT) NotDeclaredHere
```

After the fix (`CppEnumDef::get_name` reads between the keyword and the body, the scope walker no longer falls
back to a declaration's last identifier, `FORMAT_VERSION` 23):

```text
facts state_defs.h --name MODE_INSERT      25:3  MODE_INSERT  variable
hover mouse.c:894:18                      Declared in `state_defs.h:25:3`
```

Test: `an_anonymous_enum_is_not_named_after_an_initializer` (`tests/scopes.rs`). A two-line fixture,
`enum { A = 1, B = A };`, is the whole defect: the scope used to come out `Enum{A}` with `A` bound as a *type* at
the initializer's use.

**What the remaining misses are**, from the probe's examples:

* **Variables declared by a macro** — `win` from `FOR_ALL_TAB_WINDOWS(tab, win)` (`mark.c:1319`), `args` from
  `MAXSIZE_TEMP_ARRAY(args, 3)` (`deprecated.c:1001`), `watchers` in `server.c:108`. The plain reading sees a call,
  so the name has no declaration; resolving these needs the macro's expansion in the scope, which this layer does
  not have.
* **Names inside a macro definition body** (`bit.c`'s `BRET`/`BIT_OP`, `mpack`'s parser macros) — `UnparsableName`,
  while the classifier still draws them as code names. The two layers disagree about whether a macro body is code;
  that is the next round.
* **A parameter type in a multi-line signature** whose list holds a macro invocation — `Integer src_id` in
  `deprecated.c:220`, where `Dict(empty) *opts` sits later in the same list.

**`include-not-found` is a warning, and a branch nobody compiles is not a finding.** Two changes from the
editor report of 2026-11-13:

* **Severity** is `Warning` rather than `Error` — the claim is about *this analysis's* search (the compile
  database, the toolchain discovery, the include graph), and a build can pass `-I` paths through a generator or
  have an SDK where discovery cannot see it.
* **A directive in a branch this configuration does not take is silent.** neovim's `#if defined(__linux__) /
  # include <execinfo.h> / #endif` names a file no Windows build opens, so "was not found" was a claim about a
  search that should not have been made. The guard's question is answered by the index
  (`ProjectIndex::the_guard_is_taken`) and only a **decided** `Inactive` silences the finding: `Unknown` keeps it,
  the same rule the declaration lookup follows.

Measured on the 52-file sample: `IncludeNotFound` **4 → 2** (the two sanitizer headers went; `execinfo.h` and
`langinfo.h` stay because their guard is `defined(__linux__)`, which the *summary* cannot decide — it stores
questions, not answers, and the compilation environment lives in the session rather than the index; see
`docs/open-questions.md`).
### The enumerator that was a variable (2026-11-13, later)

`type_at` was 64.7% and the misses were dominated by one name kind: **enum members**. `DeclKind` has one variant
for "a name that holds a value", and the index inferred an enumerator from *a written type being absent* plus *a
scope being present* — which is exactly backwards for the shape C writes most, an anonymous
`enum { WILD_FREE = 1, … };` whose members have no scope. So the declaration card said "an enumerator of (an
anonymous enum)" while every use in another file was drawn as a `variable`: one name, two answers.

**Fixed**: `DeclFact::enumerator`, set from the binding by every fact builder (`FORMAT_VERSION` 24, `CODEC_VERSION`
40), and `IndexedKind::of` reads it instead of deriving. Measured on the same 52-file sample:

```text
                     before      after
EnumMember names        625      1 939     (+1 314 enum uses)
Variable names       12 115     10 801     (-1 314)
type_at asked           829        549
type_at known           536        536
type_at rate          64.7%      97.6%
```

The rate moves because the question got honest, not because the analysis learned to type an enumerator: a hover on
one answers the *declaration* ("an enumerator of …"), which is what a client shows, and `enum { Red };` declares
no type name to answer with. The probe now asks `type_at` only about variables, parameters and fields.

What is left is 13 misses in the sample, and they are the classes §8 already lists: names inside macro *definition*
bodies (`bit.c`'s `BRET`/`BIT_OP`), variables a macro declares (`args` from `MAXSIZE_TEMP_ARRAY`, `wp` from
`FOR_ALL_TAB_WINDOWS`, `watchers`), and a few parameters of macro-heavy signatures (`name` in `deprecated.c:742`).
**And the four `undefined-name` findings are declarations in *generated* headers.** They have no declaration in
`src` at all: neovim's build writes `build/include/…/*.h.generated.h` and the sources `#include` them like any
other header — `server_teardown` is at `build/include/msgpack_rpc/server.h.generated.h:13`, `tv_list_alloc` at
`build/include/eval/typval.h.generated.h:15`. `tests/closure_symbol_probe.rs` measured the chain:

```text
[closure] server.h.generated.h summary read: false
[closure] the closure holds the generated header: true (625 file(s))
[closure] definition server_teardown -> Unknown(NotDeclaredHere("server_teardown"))
[closure] the generated summary's own facts: None
```

So the include graph *reaches* the header (the resolver found it on disk, and `inspect facts` on it lists the
declaration), and **nothing ever read it**: it is not a project file (the scan takes `src`), and
`want_the_closure_cooked`'s nested pass is bounded by `COOK_ONE_LEVEL_FURTHER`, whose budget is spent on other
nested files of the same file first — the generated header under `msgpack_rpc/` never gets a turn.

**The obvious fix is wrong, and the suite says so.** Queueing a *read* for unread closure files (instead of
skipping them, since a file with no summary cannot be cooked) was tried and reverted: it made
`a_rendering_is_not_left_in_the_cache_for_the_next_session` fail — `definition("Live", api.h)` answered
`from_the_source`'s branch (offset 23) instead of `alone` (53) or `Ambiguous`, which is exactly the leak that test
exists for. Reading the header *inside the includer's cook* carries the includer's `#define` into what is filed
against the header. The next change is therefore not "read more files": it is the environment question one step
further out — the same arm as the paragraph above — and that test is the constraint on it.