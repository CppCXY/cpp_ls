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

## Named, registered, and not yet walking

Each of these is a module with the rule's rationale, a code, a severity and an `#[ignore]`d test stating what it should
say. They are silent today; `tests/style_checks.rs` is the to-do list, and a rule turns green when its walk lands.

```text
braces-around-statements       readability   a one-statement body should be a block
use-nullptr                    modernize     `NULL` is `0`, so `f(NULL)` picks the `int` overload
use-override                   modernize     an override without `override` is a silent non-override
simplify-boolean-expr          readability   `if (x == true)` is `if (x)`
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