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
## A language is a set, not a level — and the set is one `u64`

`CppLanguageLevel` is **one enum value**, so a project built with `-std=gnu++23` had to be configured as either `Cpp23`
(and then refuse the GNU constructs its own headers use) or `Gnu` (and then refuse `deducing this`). Every real build is a
**combination**, and the combination costs one word:

```text
bits 0..9    one per Standard: C89 C99 C11 C17 C23 | C++11 C++14 C++17 C++20 C++23
bits 10..12  one per Vendor:   GNU | MSVC | Clang
bits 13..    free — a feature that needs its own flag has somewhere to go

LanguageConfig { bits: u64 }        Copy, 8 bytes, `const fn` constructible, `#[repr(transparent)]`
  .support(feature)                 one shift and one AND
  .support_all(&[..]) / .support_any(&[..])
  .describe()                       `gnu++23`, `c++20`, `gnu89`, `c17+msvc`
  .bits() / .from_bits(u64)         for a session's stored config or a cache key
```

Every query is `self.bits & needed.covered_by_mask() != 0`, where the mask is **precomputed** (`covered_by_mask` for a
standard, `bit` for a vendor) rather than computed per call.

### The direction of the mask is the whole care of it

`support` asks "does *some enabled standard cover* the needed one?", so the mask holds the standards **at or after** it:
`C++17` is satisfied by `C++17`, `C++20` and `C++23`. The first version stored the **inverse** — the standards `C++17`
covers — which made every standard query zero, and ten of twelve tests failed at once. The two functions are both kept
(`covered_by_mask` and its inverse `covers_mask`), because "what does enabling this give me?" is a real question too.

### The C half

The same table, with a feature per release and the C++ columns left empty:

```text
C89   the baseline whose rules a later standard relaxes (C89Declarations)
C99   // comments, long long, restrict, inline, _Bool, VLAs, flexible array members, variadic macros,
      compound literals, designated initializers (shared with C++20)
C11   _Generic, _Atomic, _Static_assert, _Alignas, _Noreturn, anonymous structs, _Thread_local
C17   **no new syntax** — it is C11 with defect reports, and `C17NoNewSyntax` says so rather than leaving a gap
C23   [[attribute]], nullptr, constexpr, typeof, _BitInt, auto, enum with a fixed underlying type,
      empty initializers, #elifdef
```

Two of these exist because one feature can arrive two ways, and only the requirement can say so:

- **`typeof`** is C23 **and** GNU. That is the gap that was measured: `__typeof__` parsed and `typeof` did not, in a
  `gnu++` project where both should have been accepted.
- **`restrict`** is C99's keyword and GNU's `__restrict` is accepted in C++ mode too, so it has two variants
  (`Restrict`, `RestrictGnu`) with different requirements rather than one variant that lies for one of them.

### Tests

`tests/features.rs` — twenty cases, and three of them are about the *shape* rather than the answers:

```text
the_config_is_one_word            size 8, align 8, `Copy`
the_config_is_const_constructible  a `const GNU_CPP23` used in a `const` context
the_mask_round_trips               bits() → from_bits() → the same config, and one bit per standard
```

The rest are the combinations: `gnu++23` and strict `c++23`; C++20 covering C++17/14/11; the families not covering each
other; `gnu89`, `c11`, `c17`, `c23`; the two-route features; an unconfigured parse supporting **nothing**; `support_all`
and `support_any`; and a completeness test that builds a config from each feature's own requirement and asserts it
supports the feature — the invariant that keeps a new feature's requirement from being unsatisfiable.

## The migration

The feature set is now the vocabulary the lexer speaks, and it reached it without changing a single parse.

```text
features::LanguageConfig        one `u64`: standards | vendors, `Copy`, `const fn`
  from_flag("-std=gnu++23")     and the spellings compilers also print: c++1z, c++2a, c++latest, iso9899:1999
  from_command_line(args)       the **last** `-std=` wins, because that is what a compiler does
  from_language_level(level)    the old enum, converted — `GnuCpp` is C++17 + GNU, which is what it always meant
  permissive()                  every standard, every vendor

LexerConfig.language            new field, derived by `new` as `from_language_level(level).with_all_vendors()`
  supports_raw_strings()        → `string_prefixes && language.support(Feature::RawStrings)`
  supports_digit_separators()   → `language.support(Feature::DigitSeparators)`
LexerConfig::with_language(..)  narrows it

ParserConfig.with_language(..)  the same, one layer up
ParserConfig.with_dialect(..)   adds the vendor: a `Dialect` **is** a vendor, and the two fields can no longer disagree

lexer::name_to_kind("typeof")   the first keyword gated on a feature: DecltypeKeyword when the config says C23 or GNU,
                                an ordinary identifier otherwise
```

**The default is deliberately permissive.** `LexerConfig::new` keeps the level's standards and turns on **every vendor**,
because that is what the lexer did before the migration — its keyword table was unconditional — and a migration that
changes parses is not a migration. Narrowing is explicit: `with_language(..)` or `with_dialect(..)`.

### One test states the whole point

```rust
// the same source, two configurations
let source = "void f() { typeof(1) n = 1; }";
gnu++20    → no parse errors      (typeof is the keyword)
c++20      → parse errors         (typeof is a name, and the declaration does not parse)
c23        → no parse errors      (the feature's second route)
```

Two mistakes this stretch found, both by tests rather than by reading:

- **The old level test stayed beside the feature test** in `supports_raw_strings`, so a lexer given a **C** configuration
  but a `Cpp23` level answered "yes" to `R"(…)`. One authority or none.
- **`gnu89` parsed as `None`**: the vendor prefix was stripped and the `c` was never put back. `gnu++23` was fine, which is
  exactly the kind of asymmetry that survives a reading and dies in a test.

### What is left, after the second pass

The replacement pass is done for the two gates that were level tests in disguise:

```text
cpp_lexer.rs  binary literals   `language_level < Cpp14` → `!language.support(Feature::BinaryLiterals)`
lexer_config  string prefixes   `language_level >= Cpp11` → `language.support(Feature::RawStrings)`
lexer_config  digit separators  `language_level >= Cpp14` → `language.support(Feature::DigitSeparators)`
name_to_kind  typeof            → `language.support(Feature::Typeof)`
name_to_kind  consteval         → `Feature::Consteval`        (C++20; an identifier before it)
name_to_kind  nullptr           → `Feature::Nullptr`          (C++11 *and* C23)
name_to_kind  static_assert     → `StaticAssert || StaticAssertC`  (C++11 *and* C11)
name_to_kind  alignas           → `Feature::AlignasCpp`       (C++11; C spells `_Alignas`)
name_to_kind  thread_local      → `Feature::ThreadLocal`      (C++11; C spells `_Thread_local`)
name_to_kind  co_await/co_return/co_yield → `Feature::Coroutines`  (C++20; identifiers before)
```

Eleven features are now asked about by the lexer and grammar, up from four, and each gated word has a test asserting
**both** sides: a token says `Identifier` in a standard that predates the keyword and the keyword's own kind in one that
has it.

**One read deliberately stays**: `a_type_the_compiler_spells` in `types.rs` still matches on `dialect()`, and the reason
is written there. `__int64` is a type under MSVC and a macro under GNU, so "is this spelling a type" cannot be answered by
asking whether a vendor is *enabled* — it needs to know which compiler is *reading the file*. `support()` answers "may I
accept this syntax"; `dialect()` answers "what does this spelling mean where two compilers disagree". They are different
questions, and this is the one place the second is asked.

Only a caller that knows the compiler has the second answer, so the default parse must keep having one — which is why
`with_dialect` was not removed either. Everything else in the crate now asks the feature set.

## The flag reaches the analysis — and where it stops

`CompilerConfig::language_for(file)` answers the question nothing was asking: **what is this file compiled as?**

```text
the file's own command's `-std=`      a compilation database is per translation unit — one target C++17, another C++20
else `[compile].extra_args`           a person saying what the build is
else `LanguageConfig::permissive()`   a parse that has not been told what it is reading must not refuse what it reads

no stored field: the database is already the one place the answer comes from, and a cached copy goes stale the moment
                 either it or `extra_args` changes
```

Four tests in `crates/cpp_code_analysis/tests/language.rs`: a project's flags decide the language (last `-std=` wins, and
`gnu++20` is C++20 **plus** GNU), an unconfigured project is permissive, a command with no standard flag falls through
rather than inventing one from `-Iinclude`, and a separated `-std c++17` is read.

**Where it stops.** The parser is not yet asking. `FileView::parse_with` and the five `CppParser::parse(source,
ParserConfig::default())` call sites in `session.rs` build their configuration from nothing, so `language_for` is a
correct answer with no caller. The change is to carry the language into `ParserConfig` at those call sites — one field
already exists (`ParserConfig::with_language`), and `FileView` is built by the session, which is where the
`CompilerConfig` for the file is in hand. That is the last seam of the whole chain, and it is named here rather than left
to be rediscovered:

```text
compile_commands.json  →  CompilerConfig::language_for(file)  →  ParserConfig::with_language(..)  →  LexerConfig.language
     ✓                        ✓                                     ✗ not yet                            ✓
```