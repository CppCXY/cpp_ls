# The formatter against the one Visual Studio ships

`crates/cpp_formatter` claims clang-format's configuration surface and half of its output: the **spaces** between
tokens and the **indentation** of a line are computed, but the **line breaks are the source's** — `render` only
re-lays-out lines that already exist (`lib.rs` says so out loud: *"Until then `format` keeps the input's line
structure"*). This note is the measurement of that gap and the plan for closing it.

The oracle is `clang-format 15.0.1`, the copy under
`C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\Llvm\x64\bin\clang-format.exe` — the one Visual
Studio's own "Format Document" runs.

## The measurement

```text
$env:CPPLS_CLANG_FORMAT = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\Llvm\x64\bin\clang-format.exe"
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

### Baseline, 2026-11-20, `LLVM`, 29 files of `CodeFormatCore\src`

| run | identical files | differing lines | spacing-only |
| --- | --- | --- | --- |
| before any fix | 2 / 29 | 5 610 / 6 895 | 8 / 29 match, 195 lines differ |
| after `an empty block is {}` (`render`) | 3 / 29 | 4 931 / 6 882 | 8 / 29 match, 195 lines differ |
| after the breaker stops treating a declarator `&`/`*` as a binary operator | 3 / 29 | 4 913 / 6 882 | 8 / 29 match, 195 lines differ |

The first fix is the one defect present on nearly every file: an empty body written `{` newline `}` must come back
`{}`. clang-format does that under **every** style — `AllowShortBlocksOnASingleLine: Never` governs a block with
statements in it, not an empty one.

### The same corpus in the other two styles the settings offer (same day)

| style | identical files | differing lines | spacing-only |
| --- | --- | --- | --- |
| `LLVM` | 3 / 29 | 4 913 / 6 882 | 8 / 29 match, 195 lines |
| `Google` | 1 / 29 | 6 642 / 6 913 | 2 / 29 match, 498 lines |
| `Microsoft` | 0 / 29 | 7 658 / 7 982 | 6 / 29 match, **5 565** lines |

`Microsoft` is the one Visual Studio names as its default, and it is the worst on both numbers — for a reason that is
not a defect in a rule but a **missing rule**: the style is `BreakBeforeBraces: Custom` plus a table of `BraceWrapping`
sub-options (functions, classes, namespaces, `catch`, `else`, …), and this formatter only reads
`BreakBeforeBraces == "Attach"`. Every brace that should open the next line is written attached, so the whole file is
off by one line and every following line's diff is doubled. `BraceWrapping` is the single largest lever after the
reflow itself.

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
