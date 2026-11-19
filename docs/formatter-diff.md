# The formatter against the one Visual Studio ships

`crates/cpp_formatter` claims clang-format's configuration surface and half of its output: the **spaces** between
tokens and the **indentation** of a line are computed, but the **line breaks are the source's** — `render` only
re-lays-out lines that already exist (`lib.rs` says so out loud: *"Until then `format` keeps the input's line
structure"*). This note is the measurement of that gap and the plan for closing it.

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
| `LLVM` | 5 / 29 | 1 443 / 6 850 | 19 / 29 match, 74 lines |
| `Google` | 3 / 29 | 1 607 / 6 887 | 21 / 29 match, 68 lines |
| `Microsoft` | 5 / 29 | **487** / 8 031 | 27 / 29 match, **4 lines** |
| `BreakBeforeBraces: Allman` | 3 / 29 | 1 461 / 8 657 | 15 / 29 match, 69 lines |
| `LLVM, ColumnLimit: 0` | **11 / 29** | **226** / 6 510 | **26 / 29 match, 10 lines** |

The last row is the corpus's own style. It scores best for a reason that is also the best evidence the formatter
works: `ColumnLimit: 0` is the one setting where clang-format itself does not reflow, so the only differences left
are the token-level ones — ten lines in three files.

### Per-option rates (same corpus, `LLVM` base with one option changed)

| style / option | identical files | changed lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` (base) | 5 / 29 | 1 452 / 6 850 | 19 / 29 match, 76 lines |
| `IndentWidth: 4` | 5 / 29 | 1 729 / 7 015 | 15 / 29 match, 148 lines |
| `UseTab: ForIndentation` | 5 / 29 | 1 443 / 6 850 | 18 / 29 match, 104 lines |
| `BreakBeforeBraces: Allman` | 3 / 29 | 1 461 / 8 657 | 15 / 29 match, 99 lines |
| `AllowShortFunctionsOnASingleLine: None` | 5 / 29 | 1 424 / 6 906 | 19 / 29 match, 106 lines |
| `ColumnLimit: 0` | **11 / 29** | **226** / 6 510 | **26 / 29 match, 10 lines** |

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

## Differences that are **not** going to be fixed silently

Every one of these is a known gap with a reason, not a skipped comparison:

| gap | why it is still open |
| --- | --- |
| the line **breaks** are the source's | **measured negative result**: a whole-statement reflow (join when it fits, run the penalty search when it does not) was implemented and made the differential worse — the spacing-only column went from 53 to 2 727 lines, because the search's model has no bracket alignment, no bin-packing and no first-parameter context. It is reverted, and the project is the next one: unwrapped lines, then a fitting algorithm that models those three |
| `Stroustrup`, `WebKit`, `Mozilla`, `Linux` brace wrapping | each is a style definition of its own, not a `BraceWrapping` table; `wrapping.rs` attaches them and says so, rather than guessing |
| GNU `IndentBraces` and `BreakAfterReturnType: TopLevel` | GNU's answer (`void` newline `f ()`) is two features, measured in `cases_wrapping.rs` as a note instead of an assertion |
| `BraceWrapping.AfterControlStatement: MultiLine` | treated as `Never` (attach): sound only when the header fits on one line, and the header's width is a reflow question |
| **`AlignAfterOpenBracket` inside a nested bracket opened on a continued line** | the anchor is the line the bracket was written on, and a second nesting level still comes out one `ContinuationIndentWidth` short on a few lines (`IndentWidth: 4`: 148 spacing lines); the accumulated-indent model clang-format uses is not reproduced yet |
| `FixNamespaceComments` on an anonymous namespace | clang-format writes `} // namespace` with no name; this formatter knows only named ones |
