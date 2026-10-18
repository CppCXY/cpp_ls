# Syntax coverage: one battery per standard, one per vendor

`crates/cpp_parser/tests/` now holds a table per dialect, each in its own file, each a list of `(feature, source)` pairs
that must parse without an error. A shared `common::assert_all_parse` reports **every** construct a file fails rather than
the first, so one run says how far behind the grammar is in one number.

| file | language | constructs | result |
|---|---|---|---|
| `cpp11.rs` | C++11 | 18 | ✓ (a braced range is `#[ignore]`d) |
| `cpp14.rs` | C++14 | 9 | ✓ |
| `cpp17.rs` | C++17 | 14 | ✓ |
| `cpp20.rs` | C++20 | 17 | ✓ |
| `cpp23.rs` | C++23 | 14 | ✓ |
| `extensions_gnu.rs` | GNU | 12 | ✓ (2 gaps `#[ignore]`d — `typeof` fixed) |
| `extensions_msvc.rs` | MSVC | 12 | ✓ (3 gaps `#[ignore]`d) |
| `extensions_clang.rs` | Clang | 12 | ✓ (1 gap `#[ignore]`d) |

## What does not parse, measured

Every gap is a real construct that produces a `SyntaxError`, found by running the tables rather than by guessing:

```text
C++11   for (int x : {1, 2, 3})        expected primary expression at the `{`
GNU     int n = ({ int a = 1; a; });   expected primary expression at `({`     ← glibc macros are built on this
GNU     typeof(1) n = 1;               **fixed** — `typeof` is another spelling of `decltype` and was not lexed as one
                                       (`__typeof__` and `__typeof` were, which is the worst kind of gap: one spelling of
                                       a construct working and another not). One line in the lexer's GNU table:
                                       `"__typeof__" | "__typeof" | "__decltype" | "typeof" => DecltypeKeyword`.
GNU     int n = a ?: b;                expected primary expression at `?:`
MSVC    __try { } __except (1) { }     expected `;` after expression          ← every SEH-using header
MSVC    __try { } __finally { }        expected `;` after expression
MSVC    __interface I : IUnknown { };  expected `;` after expression
Clang   void (^block)(int) = ^(int n) { };   expected primary expression at `^`
```

Two of these are worth naming as priorities beyond their rarity: **GNU statement expressions and `typeof`** are how
glibc's own headers define `max`, `min` and every macro that declares a temporary, and **MSVC `__try`** is the shape of
every SEH-using header on Windows. A file that includes either produces parse errors, and the parser's recovery means the
analysis keeps going — but with text it has mis-grouped.

## What "parses" does not mean

`assert_all_parse` asks one question, and the honest one is narrower than "supports C++23". A construct can parse and
still be misread by the analysis: `deducing this`, for instance, parses and the member is then recorded with a parameter
list the member queries have never seen. The next layer of testing is **semantic** per construct — does `if consteval`
produce both branches, does `a[i, j]` subscript once, does `this Self&&` bind `self` — and it belongs beside these tables,
one file per standard, the same way.

## The remaining gaps, with the place each lives

Measured after the `typeof` fix, and left here so the next attempt starts from the file rather than from the symptom:

```text
({ … })        a statement expression       grammar/cpp/exprs.rs — the primary-expression parser. `(` followed by `{`
                                            needs to parse the brace as a block and the expression's value is the last
                                            statement's. glibc's `max`/`min` macros are built on it.
a ?: b         the elvis operator           grammar/cpp/exprs.rs — the conditional expression. The middle operand is
                                            simply absent; parsing it as "the true branch is empty" is the whole change.
__try/__except structured exception         grammar/cpp/stats.rs — the statement parser. Three new statement kinds
__try/__finally handling                    (`__try`, `__except (expr)`, `__finally`), each a block; `__except`'s
                                            parentheses hold an expression or the constant `1`.
^(…) { … }     a Clang block               grammar/cpp/exprs.rs — a primary expression that starts with `^`, whose
                                            body is a compound statement. A block *type* already parses.
{1, 2, 3}      a braced range               `for (int x : {…})` — the range needs a braced-init-list.
```

Each is a self-contained change in one file, and each has a test already written and `#[ignore]`d that will turn green
when it lands — which is what the ignored tests are for.