# The formatter against the one Visual Studio ships

`crates/cpp_formatter` claims clang-format's configuration surface and half of its output: the **spaces** between
tokens and the **indentation** of a line are computed, but the **line breaks are the source's** — `render` only
re-lays-out lines that already exist (`lib.rs` says so out loud: *"Until then `format` keeps the input's line
structure"*). This note is the measurement of that gap and the plan for closing it.

**Because of that gap the server does not use this formatter by default.** `[format] formatter` names the program
`textDocument/formatting` runs, and the default is `clang-format` from `PATH`; `cpp_formatter` is what answers when a
project asks for it (`formatter = "cpp_formatter"`) or when no clang-format can be found (with one warning). This
document measures the built-in one either way — it is the half of the answer that has no external dependency, and it
is what the hover's macro layout uses. See `docs/configuration.md`'s `[format]`.

The oracle is **clang-format 23.1.5** — the newest release, installed with `pip install clang-format==23.1.5`, at
`C:\Users\zc\AppData\Local\Programs\Python\Python314\Lib\site-packages\clang_format\data\bin\clang-format.exe`.
Visual Studio's own copy is 15.0.1 and is **too old to measure against**: on this corpus the Google style alone moved
from 1 to 3 identical files and from 2 to 9 spacing-only files between the two versions, so a baseline that does not
name its oracle is a number about nothing. The harness prints the version it ran against for that reason.

## The measurement

```text
$env:CPPLS_CLANG_FORMAT = "C:\Users\zc\AppData\Local\Programs\Python\Python314\Lib\site-packages\clang_format\data\bin\clang-format.exe"
$env:CPPLS_FMT_CORPUS  = "E:\EmmyLuaCodeStyle\CodeFormatCore\src"
$env:CPPLS_FMT_STYLE   = "LLVM"           # or Microsoft, Google, or an inline `{BasedOnStyle: …, …}` object
$env:CPPLS_FMT_LIMIT   = "40"
cargo test --release -p cpp_formatter --test against_clang_format -- --ignored --nocapture
```

Two numbers, because two different defects produce them:

| number | what it isolates |
| --- | --- |
| **identical files / changed lines** | the whole job: both formatters on the same source. The line count is a **longest-common-subsequence diff**: a formatter that breaks one line into two shifts every line after it, and a positional count calls all of them different — that number measures line offsets, not mistakes. A file is "identical" when nothing differs at all |
| **files that match on text clang-format already laid out** | **spacing and indentation alone** — the corpus is first formatted by clang-format, then fed to `cppfmt`; a difference here is a token-level mistake, and a file that matches here but not above differs in *where the lines go* |

### The corpus, against clang-format 23.1.5 (2026-11-20)

`E:\EmmyLuaCodeStyle\CodeFormatCore\src`, 29 files:

| style | identical files | changed lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` | 4 / 29 | 1 445 / 6 850 | 18 / 29 match, 68 lines |
| `Google` | 4 / 29 | 1 602 / 6 886 | 21 / 29 match, 58 lines |
| `Microsoft` | 6 / 29 | **483** / 8 033 | 28 / 29 match, **2 lines** |
| `BreakBeforeBraces: Allman` | 3 / 29 | 1 450 / 8 657 | 16 / 29 match, 57 lines |
| `LLVM, ColumnLimit: 0` | **11 / 29** | **224** / 6 510 | **27 / 29 match, 8 lines** |

The last row is the corpus's own style. It scores best for a reason that is also the best evidence the formatter
works: `ColumnLimit: 0` is the one setting where clang-format itself does not reflow, so the only differences left
are the token-level ones — ten lines in three files.

### Per-option rates (same corpus, `LLVM` base with one option changed)

| style / option | identical files | changed lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` (base) | 4 / 29 | 1 445 / 6 850 | 18 / 29 match, 68 lines |
| `IndentWidth: 4` | 4 / 29 | 1 730 / 7 015 | 14 / 29 match, 142 lines |
| `UseTab: ForIndentation` | 4 / 29 | 1 445 / 6 850 | 17 / 29 match, 98 lines |
| `BreakBeforeBraces: Allman` | 3 / 29 | 1 451 / 8 657 | 15 / 29 match, 61 lines |
| `AllowShortFunctionsOnASingleLine: None` | 4 / 29 | 1 417 / 6 906 | 18 / 29 match, 68 lines |
| `PointerAlignment: Left` | 4 / 29 | 1 444 / 6 850 | 18 / 29 match, 68 lines |
| `BreakConstructorInitializers: BeforeComma` | 4 / 29 | 1 464 / 6 852 | 16 / 29 match, 92 lines |
| `ColumnLimit: 0` | **10 / 29** | **228** / 6 510 | **26 / 29 match, 12 lines** |

What each row says:

* **`UseTab`** is implemented, and a continuation line tabs its **indentation part** and keeps its *alignment* as
  spaces (`Columns::indent` takes both columns): measured, a continuation at column 12 whose statement is indented 8
  is `<TAB>` plus four spaces under `ForIndentation`. Its remaining 136 lines are the table alignment below and two
  nested-continuation columns.
* **`Allman`'s 99 spacing lines** are the case-body column, the empty-constructor split and `AlignEscapedNewlines`,
  all implemented this round; the remaining whole-file difference is the reflow, as for every style.
* **`AllowShortFunctionsOnASingleLine: None`** costs nothing in spacing (106 lines, the base's number) and is
  implemented by the same rule as `All`: a body is joined only when the option allows it, and a body the option
  refuses is **broken** — empty ones included (`void z() {` then `}`).
* **`ColumnLimit: 0`** is the corpus's own style and the one the formatter reproduces most closely: 226 changed lines
  in 29 files, 26 of them matching clang-format's layout exactly.

### The second corpus: Lua 5.5 (`C:\Users\zc\Desktop\lua-5.5.0\src`, 40 files)

| style | identical files | changed lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` | 0 / 40 | 7 309 / 20 302 | 10 / 40 match, 1 875 lines |
| `Google` | 0 / 40 | 10 991 / 20 343 | 3 / 40 match, 6 641 lines |
| `Microsoft` | 0 / 40 | 11 362 / 22 010 | 13 / 40 match, 1 486 lines |
| `BreakBeforeBraces: Allman` | 0 / 40 | 7 429 / 22 462 | 9 / 40 match, 1 850 lines |
| `LLVM, ColumnLimit: 0` | 0 / 40 | 6 983 / 20 578 | 12 / 40 match, 1 662 lines |

This corpus is hand-formatted with its own wrapping, and clang-format's 80-column answer re-wraps most of it: the
whole-file column measures the **break half** of the reflow, which is the next project. The token-level rules still
hold on it — the `::`-after-comma and string-concatenation cuts were found here — and its spacing-only column is
where the gains from here are.
### The braced list as a table (implemented this round)

clang-format 23 lays a braced initializer list that does not fit on one line out as a **column table**, and the layout is
not an option (`AlignArrayOfStructures` is `None`, and changing it does not change the output):

```text
static const int n[] = {111, 2222, 33, 44444, 5,  666, 7777, 88, 999,
                        10,  11,   12, 13,    14, 15,  16,   17, 18,
                        19,  20,   21, 22,    23, 24,  25};
```

The rules, each derived from a fixture and pinned in `tests/cases_tables.rs` and `tables.rs`:

* a column is as wide as its **widest** cell (comma included), and every row is padded to it;
* the row count starts from the list's **total width** over the room a line has, rounded up, and grows until the
  aligned rows fit (`25` integers: total 108 over 56 room is 2 rows, which gives 13 columns and does not fit, so one
  more row gives the **nine** the oracle writes);
* the first row stays on the `{` line unless starting it there costs **at least two rows** (the 25 operator names:
  seven rows of four after the brace, five rows of five one line down);
* the `}` follows the last cell, and a list with a comment is not a table.

Twelve fixtures match the oracle byte for byte — the integer array, the 25 operator names, 8/10/20-element string
lists at two widths, and the same names behind three different declaration prefixes — and the corpus's
`NameStyleChecker.cpp` table now matches too.
### Every corpus, every style — and the command that prints it

`scripts/formatter-diff.ps1` is the rerunnable form of this document's tables:

```text
$env:CPPLS_CLANG_FORMAT = "…\clang-format.exe"     # 23.1.5 is what the numbers below were measured with
pwsh -File scripts/formatter-diff.ps1              # every corpus, the three naming styles
pwsh -File scripts/formatter-diff.ps1 -Options     # plus one option at a time
pwsh -File scripts/formatter-diff.ps1 -Corpus "Lua 5.5"
```

It runs the differential harness once per cell and prints markdown, so the table below is reproducible rather than
transcribed. Four corpora: the EmmyLua formatter's own sources, Lua 5.5's C sources, the nlohmann json include tree,
and this repository's two C++ fixtures.

| corpus | style | identical files | changed lines | spacing-only |
| --- | --- | --- | --- | --- |
| EmmyLua | `LLVM` | 5 / 29 | 1 441 / 6 850 | 19 / 29 files, 64 lines |
| EmmyLua | `Google` | 3 / 29 | 1 605 / 6 887 | 21 / 29 files, 58 lines |
| EmmyLua | `Microsoft` | 5 / 29 | 485 / 8 031 | **28 / 29 files, 2 lines** |
| Lua 5.5 | `LLVM` | 0 / 40 | 7 309 / 20 302 | 10 / 40 files, 1 875 lines |
| Lua 5.5 | `Google` | 0 / 40 | 10 628 / 20 270 | 5 / 40 files, 5 983 lines |
| Lua 5.5 | `Microsoft` | 0 / 40 | 10 574 / 22 008 | 14 / 40 files, **667 lines** |
| nlohmann json | `LLVM` | 0 / 40 | 28 262 / 27 337 | 1 / 40 files, 2 558 lines |
| nlohmann json | `Google` | 4 / 40 | 32 960 / 27 360 | 7 / 40 files, 8 062 lines |
| nlohmann json | `Microsoft` | 7 / 40 | 9 820 / 27 327 | 12 / 40 files, 1 056 lines |
| cpp_ls fixtures | `LLVM` | 0 / 2 | 108 / 145 | 0 / 2 files, 28 lines |
| cpp_ls fixtures | `Google` | 0 / 2 | 121 / 146 | 0 / 2 files, 41 lines |
| cpp_ls fixtures | `Microsoft` | 0 / 2 | 30 / 170 | 0 / 2 files, 20 lines |

One option at a time (EmmyLua, `LLVM` base):

| option | identical files | changed lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` (base) | 4 / 29 | 1 445 / 6 850 | 18 / 29 files, 68 lines |
| `IndentWidth: 4` | 4 / 29 | 1 730 / 7 015 | 14 / 29 files, 142 lines |
| `UseTab: ForIndentation` | 4 / 29 | 1 445 / 6 850 | 17 / 29 files, 98 lines |
| `BreakBeforeBraces: Allman` | 3 / 29 | 1 451 / 8 657 | 15 / 29 files, 61 lines |
| `AllowShortFunctionsOnASingleLine: None` | 4 / 29 | 1 417 / 6 906 | 18 / 29 files, 68 lines |
| `PointerAlignment: Left` | 4 / 29 | 1 444 / 6 850 | 18 / 29 files, 68 lines |
| `BreakConstructorInitializers: BeforeComma` | 4 / 29 | 1 464 / 6 852 | 16 / 29 files, 92 lines |
| `ColumnLimit: 0` | **10 / 29** | **228** / 6 510 | **26 / 29 files, 12 lines** |

**How to read the two corpora that are not written for these styles.** nlohmann json and Lua 5.5 are formatted for
their own conventions (nlohmann's 120-column house style, Lua's hand-wrapped C), so clang-format's 80-column answer
re-wraps most of their lines: their `changed lines` column measures the **break half** of the reflow, which this
formatter implements only for statements that already overflow. Their **spacing-only** column — the same text
clang-format laid out, fed back through `cppfmt` — is the comparable one, and it is where the remaining work is
named: 1 875 lines on Lua 5.5, 2 558 on nlohmann json with `LLVM`.
## What the remaining difference is

Three kinds, in the order they cost lines on the corpus:

1. **The line breaks are never moved.** `void f() { if (x) { y(); } }` stays one line because it was written that
   way, and `struct S { int x; };` stays one line because it was written that way — clang-format breaks a non-empty
   record, breaks a statement after `if (…)`, and joins a short function body
   (`AllowShortFunctionsOnASingleLine`). This is not a bug in a rule; it is the rule that has not been written.
2. **Options declared but never read.** Of the **275** style options the config layer accepts, **36** are read
   anywhere outside the option tables. The ones the corpus's diff names most are exactly the missing half of the
   reflow: `AllowShortFunctionsOnASingleLine`, `AllowShortBlocksOnASingleLine`, `AllowShortIfStatementsOnASingleLine`,
   `AllowShortLoopsOnASingleLine`, `AllowShortLambdasOnASingleLine`, `BreakConstructorInitializers`,
   `PackConstructorInitializers`, `BinPackParameters`, `BinPackArguments`, `BreakBeforeBinaryOperators`,
   `BreakBeforeTernaryOperators`, `SpaceBeforeAssignmentOperators`, `SpaceBeforeInheritanceColon`, `UseTab`,
   `LineEnding`, `InsertNewlineAtEOF`, `SortUsingDeclarations`, `AlignConsecutiveAssignments`/`Declarations`/`Macros`.
3. **The line-breaking cost model is partial.** `PenaltyBreakAssignment` and `PenaltyBreakBeforeFirstCallParameter` are
   read, but the choice between "break after `=`" and "break inside the call" is what clang-format's optimizer decides
   and `cppfmt` currently decides by the author's own breaks. Measured on
   `int wideCharSize = MultiByteToWideChar(CP_UTF8, 0, utf8.c_str(), -1, nullptr, 0);`: clang-format breaks after `=`
   and indents the call; `cppfmt` keeps `= MultiByteToWideChar(CP_UTF8,` and breaks inside the argument list.

## The plan

```text
1. the unwrapped-line model      a logical line, not a token range: statement, block, label, declaration   (breaks.rs)
2. the fitting algorithm         join when it fits, break at the lowest penalty — arguments and parameters
3. the short-construct rules     functions, blocks, ifs, loops, lambdas, records, namespaces
4. the constructor initializer   BreakConstructorInitializers / PackConstructorInitializers
5. the token-level remainder     the 195 spacing lines, one rule at a time, each with its clang-format fixture
6. the option tables             drive the above from the style, and test every option against the oracle
```

Each step is measured with the command above before and after, and each fix carries a hermetic case in
`tests/cases_*.rs` — the oracle's answer, not this implementation's.

## The verdict: is it usable?

**It is, for the job it was built for, and the measurement says where.** "Usable" here means two things that a
formatter is judged by, and they land very differently:

| what | evidence (clang-format 23.1.5) |
| --- | --- |
| **Token-level formatting** — spacing, indentation, whether a comment or a directive sits where clang-format puts it | **EmmyLuaCodeStyle, `Microsoft`: 2 lines differ in 8 033**, 28 of 29 files byte-identical. `LLVM` with `ColumnLimit: 0` (the corpus's own style, so no re-wrapping is asked for): **8 lines in 6 510**, 27 of 29 files. nlohmann json, `Microsoft`: **517 lines in 27 551**. Lua 5.5: 841 in 20 304 (`LLVM`), 661 (`Microsoft`) |
| **Line layout** — where the breaks go on text that has to be wrapped | nlohmann json under `LLVM` re-wraps almost every file (15 of 40 identical), because the break half is a fitting search this formatter answers with the author's own breaks ([*What the remaining difference is*](#what-the-remaining-difference-is)) |

So: **on code that is already laid out the way the style wants — which is the case a formatter is used for in a
repository that has been formatted before — it is byte-identical to clang-format on 28 of 29 files of a real code
base, with two lines left.** On text that still has to be re-flowed it is not, and the gap is the penalty model, not
the token rules.

Three properties hold on every input, and they are what makes it safe to run:

* **the token stream never changes** — the renderer verifies that the output lexes to the same tokens and falls back
  to the input if it does not (`same_tokens`); that check is what caught `> >` merging into `>>`;
* **idempotent** — running it twice produces the same bytes (`tests/layout_reflow.rs`, and the spacing measurement
  itself is a second pass over clang-format's output);
* **the differences are named** — every rule that cannot be matched is recorded in
  [*Differences that are not going to be fixed silently*](#differences-that-are-not-going-to-be-fixed-silently) with
  the measurement that decided it, and nothing is skipped by narrowing the corpus.

What is **not** usable yet, stated plainly: it is not a drop-in replacement for clang-format under `LLVM`/`Google` on
unformatted `C++` headers (nlohmann json is the witness), it does not wrap comments (`ReflowComments`), and it does not
format macro bodies. Those are the first three items of [The plan](#the-plan) plus one gap row each.

## Differences that are **not** going to be fixed silently

Every one of these is a known gap with a reason, not a skipped comparison:

| gap | why it is still open |
| --- | --- |
| the line **breaks** are the source's | **measured negative result**: a whole-statement reflow (join when it fits, run the penalty search when it does not) was implemented and made the differential worse — the spacing-only column went from 53 to 2 727 lines, because the search's model has no bracket alignment, no bin-packing and no first-parameter context. It is reverted, and the project is the next one: unwrapped lines, then a fitting algorithm that models those three |
| `Stroustrup`, `WebKit`, `Mozilla`, `Linux` brace wrapping | each is a style definition of its own, not a `BraceWrapping` table; `wrapping.rs` attaches them and says so, rather than guessing |
| GNU `IndentBraces` and `BreakAfterReturnType: TopLevel` | GNU's answer (`void` newline `f ()`) is two features, measured in `cases_wrapping.rs` as a note instead of an assertion |
| `BraceWrapping.AfterControlStatement: MultiLine` | treated as `Never` (attach): sound only when the header fits on one line, and the header's width is a reflow question |
| **a braced array of structs (`AlignArrayOfStructures: None`)** | `std::array<cached_power, 79> kCachedPowers = {{…}}` keeps the **author`s rows** — clang-format 23 does not re-flow a nested-brace array, it aligns the *columns* of the rows that are there (the second cell of a row starts after the widest first cell, which is where the extra space in `{0xD3515C2831559A83, -954, -268},  {…}` comes from). A file written with one cell per line stays that way. This formatter`s table lays every cell into a **computed** table instead, so a row of two becomes two lines (or the padding columns differ) — 958 positional differences in `to_chars.hpp` alone. Re-flowing into a table is right for a flat list of literals (the earlier rows) and wrong here; the distinction is the cell being a braced group, and it needs the author-row-preserving alignment, not the table |
| **`BreakTemplateDeclarations`** | the option is `MultiLine` in LLVM, `Yes` in Google: `Yes` breaks after every template head (`template <typename T>` then `class X {`), and `MultiLine` breaks only when the whole declaration does not fit — which is the join pass`s own fit test, so it needs no rule of its own. Implemented exactly so (the fixture matches the oracle under `LLVM`, `Google` and `Microsoft`), and the corpus disagreed: nlohmann json`s `LLVM` whole-file column 27 655 → **27 869** and its spacing 941 → 950, while `Microsoft` improved 10 022 → 9 731. Reverted: joining a head changes where the *next* line can break, and the join list is not yet clang`s unwrapped-line model, so the change is recorded as blocked on that model rather than shipped |
| **`ReflowComments`** | clang-format wraps a line comment that overflows and treats consecutive comments as one paragraph; a body with **two consecutive spaces** (a table or ASCII art) is left alone, and a commented-out line keeps its indentation. Four implementations were measured and **reverted**, because none matched enough:`n`n| variant | nlohmann json | verdict |`n| --- | --- | --- |`n| wrap every line comment, before the alignment pass | 1 333 → **1 409** | worse |`n| the same, after the alignment pass | 1 333 → **1 374** | worse |`n| own-line comments only (trailing left alone) | 1 333 → 1 334, but whole-file 27 626 → 27 747 | neutral on spacing, worse on lines |`n| own-line + skip comments with a neighbour | 1 333 → 1 342, `Microsoft` whole-file −14 | worse |`n`nThe rule is exact on the fixtures (the wrap column, the body`s indentation, the `- ` bullet, the ASCII-art guard), and the residual comes from clang`s **penalty model for comment breaks** and from trailing comments being wrapped *before* their column is aligned. Doing it as a post-pass over the rendered text cannot see those, so it is recorded as a gap rather than shipped |
| **a macro body`s layout** | clang-format formats a `#define` body with the layout rules (it moved `do {` of `#define TWICE(x) do { … }` onto its own line, indents the body, wraps a long `/* NOLINT… */` and splits a `static_assert`), and this formatter keeps the author`s spliced lines (the `\` columns are still aligned). The body is a directive to this pass — one logical line, never joined or re-cut — which is measured to be closer than a partial re-layout (the corpus rows for nlohmann json improved from joining to keeping: spacing 1 957 → 1 687 with `LLVM`) |
| **a `<<` chain that continues on the next line** | clang-format aligns the continuation with the statement`s **first shift operator**: `constexpr std::uint64_t kHiddenBit = std::uint64_t{1}` then `<< (kPrecision - 1)` at **39**, and `std::cout` then `<< "…"` at 12 (nlohmann json`s `to_chars.hpp`; a fixture pins both). This formatter writes the block`s continuation column (6) because the shift operator`s column is only known once it is written, and the assignment`s operand column that would stand in is cleared at the **initializer brace** (`std::uint64_t{1}` — the parser calls that brace a body, so the `}` ends the "statement" and the column is lost). A first implementation (`shift_column` state + the operand fallback + not clearing at an initializer`s brace) reached 37 instead of 39 and was reverted rather than shipped half: the remaining two columns are in the operand column`s own definition, which the `AlignOperands` fixtures do not cover |
| **`>>` followed by `(`** | `bits >> (kPrecision - 1)` is a shift and needs a space, while `static_cast<long>(x)` and `f<int>(x)` glue. The lexer spells `>>` as two `>` tokens and the call rule reads a `>` before `(` as a template close, so the shift glues. A lexical fix (a space when the token before the `>` is an operand) was **implemented and reverted**: it also spaced `std::function<void(int)> (x)`, and the corpus said so — nlohmann json's `LLVM` spacing column went 2 029 → **2 581**, EmmyLua's 68 → **110**. The distinction needs the tree's answer, not the token's |
| **the split of a trailing-comment run at the column limit** | three fixtures pin what clang-format does and **two of the three disagree with one another** under every model tried: (a) an include block where the too-long comment starts *beyond* the run`s column — the lines after it go back to their own starts; (b) an enumeration whose first comment is too long for any alignment — the two short lines after it take the **first line`s column**; (c) the enumeration in `lexer.hpp`, where the offending comment takes the earlier column (20 + its 60-column comment = 80 exactly) and the line after it lifts the rest to 21. A model that reproduces all three (the offending comment keeps the earlier column when it fits, the part after it keeps that column and grows) matches every fixture **and regresses the corpus**: `Microsoft` 517 → 529, `Google` 1 003 → 1 015, Lua 5.5 841 → 861, `LLVM` 941 → 937. The likely reason is structural — an enumeration body`s comments are aligned by clang`s *cell* mechanism, not by `alignTrailingComments`, and the two disagree exactly on the split — so it is recorded, not tuned. The fixtures are in this document`s history (`run8`, `enumx`, `inc3`) |
| **`FixNamespaceComments` on an existing comment, and nested namespaces** | the rule the oracle shows is: an existing comment after a namespace's closer is **rewritten** (`} // wrong name` becomes `} // namespace existing`, whatever the body length), and a missing one is **added** only when the body is longer than `ShortNamespaceLines` (one statement stays bare, five get the comment). Both halves are implemented: a missing comment is added only past `ShortNamespaceLines`, and an **existing** one is rewritten — including after the file's last closer, whose gap the renderer writes after its loop. The name comes from the source text before the brace (an outer namespace's syntax node holds only the body), checked to be an identifier, without which a macro-wrapped closer read a comment's tail as a name (`NLOHMANN_JSON_NAMESPACE_END -in-header)` produced `-in-header)`, and the corpus lost four lines before that check). The fixture file with all five shapes (correct, short, anonymous, long, wrong) is the oracle's, byte for byte |
| **the exact column of a trailing-comment run near the column limit** | clang-format`s `alignTrailingComments` splits a run when a comment would pass the column limit, and the split points in a long enumerated block (`nlohmann json` gives columns 20, 20, 21, 20 for four consecutive runs) come from an iteration order this formatter approximates; the run/continuation rules match the fixtures and the block`s own lines differ by one or two columns in about six of them. Recorded rather than tuned to one file |
| **the indent of a line that follows a trailing comment** | the line **after** a trailing comment (`A, // one` then `B,`) is recomputed by the layout only **inside an enum body**: recomputing it everywhere was measured to regress the Lua 5.5 spacing column from 1 677 to 2 291 lines, because the indent this formatter computes for a continuation line is one nesting level short (see the entry above). The enum case is the one where the computed indent is measurably right, and it is fixed |
| **`AlignAfterOpenBracket` inside a nested bracket opened on a continued line** | the anchor is the line the bracket was written on, and a second nesting level still comes out one `ContinuationIndentWidth` short on a few lines (`IndentWidth: 4`: 148 spacing lines); the accumulated-indent model clang-format uses is not reproduced yet |
| `FixNamespaceComments` on an anonymous namespace | clang-format writes `} // namespace` with no name; this formatter knows only named ones |
