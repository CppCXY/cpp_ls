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

**Half of it is not**: *which* units are held depends on what the session has cooked, so a project-wide run can
still read the same header through different translation units — two consecutive census runs of the same binary
gave 2 and 3 files with parse errors, and `grid.h` had 5 errors in one and 12 in the other. The choice has to come
from the **index** — the first translation unit the compile database says compiles this header — rather than from
what happens to be in memory. That is the next step, and the reason this section exists.

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
