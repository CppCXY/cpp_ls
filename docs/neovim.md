# neovim, read the way an editor reads it

A census of `C:\Users\zc\Desktop\neovim_test\neovim` (517 files under `src`, 218 of them `.c`), asked through the
same session the server uses. The probe is `crates/cpp_code_analysis/tests/neovim_probe.rs`:

```text
cargo test --release -p cpp_code_analysis --test neovim_probe -- --ignored --nocapture
```

The checkout has a `compile_commands.json` with 218 entries (`-std=gnu11`, `-Ibuild/include`,
`-Ibuild/src/nvim/auto`), so the language and the include search are what a build would use. The numbers below are one
run of that probe; nothing is estimated.

```text
517 file(s) diagnosed, 28 with parse errors, 0 without a reading, 0 with unplaced errors, in 184 s
parse errors by message (top 9)
      23  expected `;`                       e.g. src/coverity-model.c:140
      18  expected primary expression        e.g. src/nvim/autocmd.c:1343
      11  expected a type specifier          e.g. src/nvim/map_defs.h:52
       7  expected `;` after expression      e.g. src/lpeg/lptree.c:1308
       2  expected ), but get identifier     e.g. src/nvim/os/env.c:323
       2  expected }, but get ;              e.g. src/nvim/map_glyph_cache.c:75
       2  expected }, but get identifier     e.g. src/nvim/map_value_impl.c.h:19
       1  expected `}`                       e.g. src/nvim/marktree.c:433
       1  unexpected token                   e.g. src/nvim/marktree.c:443
check findings by code
     258  UndefinedName            114  BracesAroundStatements     12  MissingInclude
     177  IncludeNotFound           94  ElseAfterReturn             9  Redefinition
                                  67  MacroParentheses            7  Unused
      15  AssignTypeCheck          14  MacroRedefined              3  MissingReturn, 1 ReturnTypeMismatch
```

Three separate problems are behind those numbers, and they need three separate fixes.

## 1. A C++ keyword used as a C name

C has no `new`, `delete`, `this`, `operator`, `template`, and no alternative tokens (`compl`, `not`, `and`, …). C++ has
all of them, and the parser has **one keyword table for both languages** — so a C file that uses one of those spellings
as a name cannot be read.

What the project writes (31 `new`-as-a-name sites, 3 `delete`, 13 `compl`; **13 of the `new` lines are in a file that
does not parse**, and so are the `delete` and `compl` ones):

```c
src/nvim/autocmd.c:1343       Object new = BOOLEAN_OBJ(new_val);          // `new` is the object
src/nvim/marktree.c:2141      MTPos new = { new_row, new_col };
src/nvim/tui/termkey/driver-ti.c:197   struct trie_node_arr *new = …;      // and `new->arr[…]` at 200
src/nvim/marktree.c:392       void marktree_intersect_pair(…, bool delete)
src/nvim/insexpand.c:4116     compl = compl->cp_next;                     // `compl` is "completion" there
```

How it breaks, measured with `inspect` on fixtures of the same shape:

```text
int new = 0;              error: expected primary expression
int delete = 0;           error: expected primary expression
int this = 0;             error: expected primary expression
int operator = 0;         error: expected primary expression
int template = 0;         error: expected primary expression
int class = 0;            no error          ← so the fix is a list of spellings, not "every keyword"
void f(int delete) { }    error: expected primary expression
```

The `marktree.c` case is why one keyword costs a whole file: `bool delete` makes the *declaration* reading fail, the
expression reading takes the `{`, and the function body (12 454..77 821 of the file) becomes one expression statement.
Everything after it is a cascade, which is most of the 28 files' errors.

There are **two** places the keywords live, and both need the gate:

* `crates/cpp_parser/src/lexer/cpp_lexer.rs::name_to_kind` — the keyword table (`new`, `delete`, `this`, `operator`,
  `template`, `class`, `namespace`, `typename`, `using`, `throw`, `try`, `catch`, …);
* `crates/cpp_parser/src/grammar/cpp/exprs.rs:136-160` — the alternative-token rewriting, which turns `compl`, `not`,
  `and`, `or`, `bitand`, `bitor`, `xor`, `not_eq`, `and_eq`, `or_eq`, `xor_eq` into operators before an expression is
  read.

The language *is* known where the cooked reading is parsed (`FileView::parse_rendering_in` passes
`config.language_for(path)`, and `LanguageConfig::family()` answers `C` for `-std=gnu11`). It is **not** passed in two
other parse paths, which is the rest of the fix:

* `FileView::parse_with_cuda` — the buffer view a hover or a completion reads (`session/mod.rs:5428` and `:1462`);
* `FileIndexer::index_unit_rendering` — the unit reading (`index/mod.rs:599`), whose `ParserConfig` has the dialect and
  the CUDA flag but no language.

So the fix is: a `is_c()` gate in the two keyword tables, the language carried into the two paths that lack it, and a
test for each direction (a C file declaring `new`, a C++ file writing a `new` expression).

### Fixed, and measured on the same checkout

The gate is in `CppLexer::name_to_kind` (`a_word_c_does_not_reserve`) and in
`the_alternative_operator`; the language reaches the buffer view (`FileView::parse_with_cuda` now takes it, fed from
`Session::config_for_file`) and the unit reading (`index_unit_rendering` passes `language_for(root)`). The fixture
measurements now read:

```text
int new = 0;                                  C: no error      C++: error (the keyword)
void f(int delete) { }                        C: no error
int compl = 0; … compl = compl + 1;           C: no error
struct T { int x; }; struct T *new = 0; …     C: no error      `new->x` is a member access
```

And the census of the same 517 files:

```text
                        before   after
files with parse errors     28      17
parse errors                59      26
`new` lines unparsed        13       0
expected primary expression 18       2
files whose first error is a `new` line  7   0
```

Tests: `cpp_parser/tests/lexer.rs::a_word_cpp_reserves_is_a_name_in_c` (the token kinds in both languages),
`cpp_parser/tests/c_language.rs` (the declarations themselves), and
`cpp_code_analysis/tests/checks.rs::a_c_file_may_name_things_cpp_reserves` (through `Session::diagnostics`, so the
language plumbing is what is under test, not just the lexer).

## 2. The build tree was never built

Everything libuv, Lua and neovim's own code generator produce is missing or empty, and the analysis reports it file by
file:

```text
build/include/uv.h                             absent        61 × `<uv.h>` was not found          66 files include uv.h
build/include/{lua,lauxlib,lualib}.h           absent        30 × <lauxlib.h>, 29 × <lua.h>, 1 × <lualib.h>
build/src/nvim/auto/memory.h.generated.h       2 bytes       → `xfree` is undeclared in 41 places
```

`build/src/nvim/auto` holds every `*.generated.h` the build writes, and they are all empty — the code generator never
ran. `src/nvim/memory.h` includes `"memory.h.generated.h"` and it resolves (the include search is right), so the
declarations are simply not in the file:

```text
[nvim] src/nvim/bufwrite.c → uv.h is Unknown(UnresolvedInclude("uv.h"))
[nvim] include-not-found: 177      undefined-name: 258 (xfree 41, Visual 10, msg_grid 7, verbose_enter 5, …)
```

This is the checkout, not the analysis: a `make deps` (cmake.deps) populates `build/include` and the generated headers,
and those 177 + a large part of the 258 go away. Two things are worth changing on our side anyway:

* the findings are *true* but not actionable one at a time — 177 squiggles for "the build tree is not there" is how a
  reader learns to ignore the channel. A single note ("this file includes `uv.h`, which is on the compile database's
  include path but not on disk — the dependencies have not been built") is the same fact once.
* `xfree` (41 findings) is not an include miss at all: the header resolves and is empty. `undefined-name` cannot tell
  that from a name nobody declares.

## 3. Grammar gaps that are not keywords

After the keyword class is set aside, the first error of the remaining files is these:

### Parenthesized declarators followed by an initializer (8 files)

```c
src/nvim/cmdhist.c:35          static histentry_T *(history[HIST_COUNT]) = { NULL, … };
src/nvim/mapping.c:69          static mapblock_T *(maphash[MAX_MAPHASH]) = { 0 };
src/nvim/option.c:210          static char *(names[3]) = { "TMPDIR", … };
src/nvim/syntax.c:169          static char *(spo_name_tab[SPO_COUNT]) = …
src/nvim/version.c:4109        static char *(lines[]) = { …
src/nvim/normal.c:4901         static const char *(ar[]) = { …
src/nvim/ex_docmd.c:4247       static char *(p_bad_values[]) = { …
src/nvim/highlight_group.c:76  static char *(hl_name_table[]) = …
```

Measured minimal shapes:

```text
int (a[3]);                    no error
int *(a[3]);                   no error          ← the declarator itself is fine
static char *(lines[]) = {};   error: expected `;`
```

The declarator is read, and then the `=` is not: the branch that handles a parenthesized declarator is entered from
`at_an_operator_led_parenthesized_declarator` (which requires a `(` **after** the `)`, i.e. a function type) or from
`(` at the cursor when no type name has been read. `static char *` is not either shape, so `parse_declarator_with`
takes `(lines[])` for an abstract declarator and the initializer is left over. `int *(a[3]);` parses only because
nothing follows it.

The same gap in a parameter list is `src/lpeg/lptree.c:1308`:

```c
static void createcat (lua_State *L, const char *catname, int (catf) (int))
```

### A cast applied to a prefix operator (1 file)

```c
src/mpack/mpack_core.c:280     byte_idx = (mpack_uint32_t)--remaining;
```

`(T)--x` is a cast whose operand starts with a prefix operator; the cast's operand is read as a primary expression.

### A macro defined in the arm that was not taken (2 files)

```c
src/nvim/os/env.c:323          extern char **environ;
src/nvim/os/pty_proc_unix.c:302  extern char **environ;
```

Both sit under `#ifdef HAVE__NSGETENVIRON`'s `#else`, and the `#if` arm writes `#define environ (*_NSGetEnviron())`.
The expander applies that macro to the live arm, so the declaration arrives as `extern char **(*_NSGetEnviron());` and
the parser reports ``expected `)` ``. That is a conditional-macro bug (`HAVE__NSGETENVIRON` is not defined on this
platform), not a keyword one.

### Left to look at

* `src/nvim/map_defs.h:52` — `static const MTDamagePair value_init_MTDamagePair = MTDAMAGE_PAIR_INIT;`, where
  `MTDamagePair` is the typedef at `src/nvim/types_defs.h:85`. The file's **first** error, so not a cascade; why the
  typedef is not a type here is not yet measured.
* `src/nvim/map_glyph_cache.c`, `map_key_impl.c.h`, `map_value_impl.c.h` — templates instantiated by `KEY_NAME(...)`
  macros over the generated headers, which are empty (problem 2).
* `src/coverity-model.c:139` — `return __coverity_alloc_nosize__()` with no semicolon. Invalid C, not ours.
* `src/nvim/drawline.c` — the census counted one parse error, and `inspect diagnostics --cook` on the same file answers
  **0**. Two readings of one file disagreeing is worth a look on its own: the session's reading was made while 517
  files were being indexed, and the single-file cook was not.

## 4. What is noise rather than a defect

`BracesAroundStatements` (114), `ElseAfterReturn` (94) and `MacroParentheses` (67) are style rules, and they fire on a
C codebase exactly as their clang-tidy namesakes do. They are not wrong — but on a project that has not adopted them
they are most of what a reader sees, which is the reason `[diagnostics] disable` exists. `MissingInclude` (12) is the
rule this session already fixed once; two of its findings on this project are worth checking against the new
header-fragment rule.

## 5. The order to fix them

| # | What | How much | Where |
|---|------|----------|-------|
| 1 | C++ keywords in C | 13 `new` lines + 3 `delete` + 13 `compl`; whole bodies misparsed | lexer table, alternative tokens, two parse paths |
| 2 | Parenthesized declarator + initializer | 8 files + the `int (catf)(int)` parameter | `at_an_operator_led_parenthesized_declarator` / `finish_init_declarator` |
| 3 | Macro from an untaken arm applied to the live one | 2 files | the conditional walk |
| 4 | Cast of a prefix operator | 1 file | cast operand reading |
| 5 | The build tree is unbuilt | 177 + ~200 findings, one fact | a note instead of a squiggle per file |

Problem 1 is the one an editor feels: it is not a squiggle, it is a file that cannot be read, and every question about
it — hover, jump, completion, rename — is answered from a tree with one expression statement where a function was.
