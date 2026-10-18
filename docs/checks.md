# Checks: what the analysis has, and what it needs

The analysis has fourteen rules that are about the **program** (a type that does not fit, a member that is not there, a
body that never returns) and now six that are about the **code** — the layer clang-tidy owns. This is the map, with what
each rule costs and what is actually implemented.

## Implemented

```text
syntax-error              the file does not parse
param-type-check          an argument does not convert to its parameter
assign-type-check         a value does not convert to what it is assigned to
undefined-field           a member the type does not declare
unused                    a declaration nothing uses
unnecessary-branch        a branch that cannot run
argument-count            too many or too few arguments
return-type-mismatch      a return that does not convert
macro-redefined           one macro, two bodies
include-not-found         a `#include` nobody can resolve
access-invisible          a member reached from where it may not be
missing-return            a body that never returns what it promises
assignment-in-condition   `=` where `==` was meant
module-note               a note about an import, not a complaint
```

## Implemented, from the style layer — all six

```text
use-nullptr              `NULL` is `0` in every standard header, so `f(NULL)` picks the `int` overload where one exists.
                         Over the expressions whose text is exactly `NULL` — the spelling is the whole evidence.
simplify-boolean-expr    `if (x == true)` is `if (x)`. Over a condition whose text contains `== true`/`== false`/`true ==`.
braces-around-statements a one-statement body should be a block. Over an `if` whose then-branch text does not begin `{`
                         — the dangling-`else` shape is the bug behind the rule, and `if` is where it lives.
else-after-return        An `else` after a branch that returns. Over an `if` whose then-branch says `return` and which has
                         an `else`. Text rather than a typed walk for `return` — `descendants()` is untyped in this parser,
                         and a `return` in a nested lambda is the refinement the typed walk would make.
cstyle-cast              A C-style cast can remove `const` without saying so. A scan of the text: `(` a type `)` then
                         something that is not an operator, with the statement keywords excluded. The parenthesis the tree
                         holds also holds the operand, and its node kind does not say "cast" — which is why this is a scan.
use-override             An override without `override` is a silent non-override. Over a class **with bases**, a member
                         that says neither `override` nor `final`, **whose name a base actually declares** — the last clause
                         is what keeps a freshly written virtual that overrides nothing out of the report, and `members_of`
                         is the query that answers it. The first version also required the member to write `virtual`, which
                         was wrong: an override need not repeat it.
```

All six are in `tests/style_checks.rs` with a positive and a negative case, and all six were scaffolded with an
`#[ignore]`d test that turned green when the walk landed.

## The second batch

```text
use-auto                 an iterator declared by its full spelling — `std::vector<Widget>::iterator it`. Narrowed from
                         clang-tidy's rule to the case that is never wrong: `::iterator` and `::const_iterator` followed by
                         an identifier, the two spellings every standard container defines.
macro-parentheses        a `#define`'s replacement list without its own parentheses, so a caller's expression is spliced
                         in at the wrong precedence. **Not** a scan of the source: `context.source` is the *reading*, where
                         a macro use is already expanded and the directive is gone (measured: a file beginning
                         `#define TWICE(x) x * 2` reaches a check as `"int f(int n) { return n + 1*2; }"`). The facts carry
                         the replacement list's range, and `context.written` is the file as its author wrote it.
```

**The `source` / `written` distinction is worth remembering.** A check that reads `context.source` sees the program after
preprocessing — the right text for a question about a program, and the wrong text for a question about a directive. One
field over, `context.written`, is what the author typed.

Both are text-level on purpose: the shape *is* the spelling, and a spelling this simple needs no tree to read. Both were
scaffolded with an `#[ignore]`d test and turned green when the walk landed — the pattern the rest of the list follows.

## Named, registered, and not yet walking

Each of these is a module with the rule's rationale, a code, a severity and an `#[ignore]`d test stating what it should
say. They are silent today; `tests/style_checks.rs` is the to-do list, and a rule turns green when its walk lands.

```text
braces-around-statements       readability   a one-statement body should be a block
use-override                   modernize     an override without `override` is a silent non-override
else-after-return              readability   an `else` after a return is a nesting that is not needed
cstyle-cast                    cppcoreguidelines   a C-style cast can remove `const` without saying so
```

**Why this order.** The six are the ones that are (a) pure syntax or pure local semantics, so they need no new index
query, and (b) each has a bug behind it rather than a taste — the `NULL` overload, the silent non-override, the `const`
removal. A style rule that is only taste can wait; these are the ones where the diagnostic prevents a defect.

## The next ones, in the order they are worth writing

```text
modernize-use-auto              iterator declarations   `for (std::vector<int>::iterator i = …)` → `auto i`
readability-implicit-bool-conv  `if (ptr)` and `if (count)` in one codebase, meaning different things
performance-for-range-copy      `for (auto x : v)` where the copy is not meant
bugprone-macro-parentheses      a macro body that needs its own parentheses
cppcoreguidelines-init-vars     a variable declared and not initialised
modernize-use-equals-default    `~S() {}` → `~S() = default`
readability-redundant-decl      one declaration twice in a file
performance-unnecessary-copy    `const std::string s = "literal";` where a `string_view` would do
```

## What a check costs, and what it may ask

`CheckContext` is deliberately **not** a session: a check sees one file's summary, its scope tree, the index as it stands,
and the semantic model — and it cannot ask for a parse, because a diagnostics channel whose cost is "one parse per check
per keystroke" is a channel that gets removed. That is the constraint the list above is ordered by: a rule that needs a
parse of another file is a rule that waits for an index query, not a rule that reads it itself.

The registry is `sema::check::run`, and adding a rule is four lines there plus a module plus a code — see any of the six
scaffolded files for the shape.

## Two texts, one crossing — and the correction that came out of asking about it

`CheckContext` carries **two texts**, and they are different documents:

```text
context.source    the reading    the cook's output: comments stripped, macros expanded, a header inlined
context.written   the file       what the author typed

a range from `own::<…>()`          → `add(…)`             converted from reading to file
a range from the summary / facts   → `add_in_the_file(…)` already a file offset
```

**Measured, and I had it wrong first.** A scan of `context.source` for `(int)` in

```cpp
#define CAST(x) (int)(x)
int f(int n) { return CAST(n); }
```

reports the cast at the **call site**, not on the `#define` line — and I wrote a test asserting that was a bug. It is not:
`add` converts reading coordinates to file coordinates, and the crate documents the choice ("a token a macro produced
reports *where the macro was invoked*, which is the place the reader can see"). The test was wrong; the design is right.

What **was** worth changing is which text the two scan-based rules read. `cstyle_cast` and `use_auto` ask about a
**spelling** — where a cast or an iterator is *written* — and that is the file, not the reading. They now scan
`context.written` and report with `add_in_the_file`, so a cast inside a `#define` is reported on its own line, which is the
line a reader edits.

### The lexer reaches the check layer

Scanning `written` has the hole the reading did not: comments and literals. A comment that says "we removed `(int)` casts"
and a string containing `"NULL"` are prose and data, and a text scan cannot tell. That hole was measured with a test before
it was fixed:

```text
cstyle_cast over `written`   → reported `(int)` inside `// we removed (int) casts everywhere`   ✗
```

`CheckContext::comments_and_literals()` lexes the file once and hands back the ranges of every comment, string and
character literal; `in_a_comment_or_literal(offset, ranges)` is the question a scanning rule asks. It is one lex per rule
per run — a lex is not a parse, and two per file is not the cost this layer is written against.

So the three layers now have one job each:

```text
parser   the structure — `own::<CppIfStat>()`, and every range that comes from a node
facts    what the preprocessor recorded — `summary.macros`, `body_range`
lexer    which bytes are code — `comments_and_literals`
model    what a type means — `members_of` for `use-override`
text     the spelling inside a node, for the rules whose subject *is* a spelling
```