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
| `LLVM` | 3 / 29 | 4 611 / 6 882 | 8 / 29 match, 195 lines |
| `Google` | 3 / 29 | 6 630 / 6 913 | 9 / 29 match, 176 lines |
| `Microsoft` | 3 / 29 | 7 391 / 8 376 | 7 / 29 match, 6 323 lines |

How `LLVM` moved, measured against 23.1.5 as each fix landed:

| run | identical files | differing lines |
| --- | --- | --- |
| before any fix | 2 / 29 | 5 610 / 6 895 |
| an empty block is `{}` | 3 / 29 | 4 931 / 6 882 |
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
