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
| **identical files / differing lines** | the whole job: both formatters on the same source |
| **files that match on text clang-format already laid out** | **spacing and indentation alone** — the corpus is first formatted by clang-format, then fed to `cppfmt`; a difference here is a token-level mistake, and a file that matches here but not above differs in *where the lines go* |

### The corpus, against clang-format 23.1.5 (2026-11-20)

| style | identical files | differing lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` | 4 / 29 | 3 963 / 6 850 | **17 / 29 match, 90 lines** |
| `Google` | 3 / 29 | 6 508 / 6 887 | **17 / 29 match, 86 lines** |
| `Microsoft` | 5 / 29 | 4 474 / 8 041 | **22 / 29 match, 38 lines** |

The `Microsoft` spacing-only column collapsed from 6 323 lines to **42** in one step, and two rules did it: a `case`
body`s brace is the **label`s** option (`AfterCaseLabel` — Microsoft attaches `case 1: {`, and this formatter was
wrapping every case body), and a file`s own header must be promoted out of its block by **name** — the CLI`s stdin
path never passed the `-assume-filename` stem through, so `#include "SpaceAnalyzer.h"` was sorted as text while
clang-format promoted it. Both are pinned in `tests/cases_wrapping.rs`.

How `LLVM` moved, measured against 23.1.5 as each fix landed:

| run | identical files | differing lines |
| --- | --- | --- |
| before any fix | 2 / 29 | 5 610 / 6 895 |
| an empty body: `{}` for a function/record, `{\n}` for a control block | 3 / 29 | 4 931 / 6 882 |
| the breaker stops treating a declarator `&`/`*` as a binary operator | 3 / 29 | 4 913 / 6 882 |
| `BreakBeforeBraces` / `BraceWrapping` / `FixNamespaceComments` | 3 / 29 | 4 611 / 6 882 |

**This round's brace work** is what moved `Microsoft` from 0 to 3 identical files: the style is
`BreakBeforeBraces: Custom` plus a `BraceWrapping` table, and the table's five answers for five constructs (a
function's brace wraps, a `union`'s attaches, a control statement's wraps, `else`/`catch` open a line, an empty record
splits) are now read from the tree — the construct a brace opens is classified in `roles.rs`, and `wrapping.rs` asks
the style. `FixNamespaceComments` writes `} // namespace ns` where the style asks for it.

What is still wrong for `Microsoft` is not the brace *placement* but the **reflow around it**: a body written on one
line stays there, an initializer list the author broke stays broken, and a short function clang-format would keep on
one line is broken by our line structure. The 6 323 spacing-only lines are that plus the token-level remainder below.
### Per-option rates (same corpus, clang-format 23.1.5, `LLVM` base with one option changed)

| style / option | identical files | differing lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` (base) | 4 / 29 | 3 963 / 6 850 | 17 / 29 match, 90 lines |
| `IndentWidth: 4` | 3 / 29 | 4 945 / 7 015 | 14 / 29 match, 136 lines |
| `UseTab: ForIndentation` | 3 / 29 | 4 589 / 6 850 | 7 / 29 match, **1 649** lines |
| `BreakBeforeBraces: Allman` | 2 / 29 | 6 651 / 8 655 | 6 / 29 match, **2 344** lines |
| `AllowShortFunctionsOnASingleLine: None` | 4 / 29 | 4 040 / 6 906 | 17 / 29 match, 90 lines |
| `ColumnLimit: 0` | **6 / 29** | 4 433 / 6 506 | 10 / 29 match, 4 524 lines |

What each row says, in the order the fixes will come:

* **`UseTab` is not implemented at all** (the option is declared and never read): every indented line is spaces where
  clang-format writes tabs, which is why the spacing-only column jumps to 1 688.
* **`Allman`'s 3 282** are mostly `AlignEscapedNewlines`: clang-format pads a macro's `\` to the column limit
  (`AlignEscapedNewlines: Right`), this formatter pads to the longest line of the macro. Measured on
  `LuaDiagnosticStyle.cpp`: the same `#define`, two different `\` columns.
* **`AllowShortFunctionsOnASingleLine: None` is now implemented** — 4 of 29 identical, 90 spacing-only lines — by
  the same rule as `All`: a body is joined only when the option allows it, and a body the option refuses is *broken*,
  empty ones included (`void z() {` then `}`). The option matrix is the record of what each setting costs.
* **`ColumnLimit: 0` scores best on identical files (6 / 29)** because that is the one setting where clang-format
  itself stops reflowing — and the remaining spacing lines there are the same macro continuations.
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
| the line **breaks** are the source's | the reflow layer is the next step (the table above); every corpus number is dominated by it |
| `Stroustrup`, `WebKit`, `Mozilla`, `Linux` brace wrapping | each is a style definition of its own, not a `BraceWrapping` table; `wrapping.rs` attaches them and says so, rather than guessing |
| GNU `IndentBraces` and `BreakAfterReturnType: TopLevel` | GNU's answer (`void` newline `f ()`) is two features, measured in `cases_wrapping.rs` as a note instead of an assertion |
| `BraceWrapping.AfterControlStatement: MultiLine` | treated as `Never` (attach): sound only when the header fits on one line, and the header's width is a reflow question |
| `FixNamespaceComments` on an anonymous namespace | clang-format writes `} // namespace` with no name; this formatter knows only named ones |
