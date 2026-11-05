# The parser, reviewed

A review of `cpp_parser` as it stands: what it is made of, what it costs, whether `rowan` is the right foundation,
what the code does well and badly, and what is worth doing next. Every number here is measured by
`crates/cpp_parser/tests/parse_cost.rs` on two real corpora — the project's own sources and the MinGW 15.1 libstdc++
headers — and can be reproduced with the command at the end.

**The verdict up front.** The foundation is sound and the grammar is unusually good: 114 real project files, **one**
with a syntax error, and every one of them lossless. `rowan` is a defensible choice and this code uses it well — the
cost that remains is **the grammar's, not `rowan`'s** (88 % of the parse on template-heavy code), so replacing the
tree library would buy a small fraction of what tuning the grammar can. Three things are worth fixing, in this order:
a **losslessness bug inside `extern "C" { }` blocks** (§4, bytes silently disappear), the **re-parse per request**
(§5.3, a view is rebuilt from scratch on every hover), and the **fragile checkpoint/speculation state** (§5.2).

## 1. What it is

```text
lexer            CppLexer over a Reader (Peekable<Chars>)  →  Vec<CppTokenData { kind, SourceRange }>
                 maximal munch, then `>`-family split once at the end of phase 3
parser           hand-written recursive descent, clang-informed, one token of lookahead plus checkpoints
                 output: Vec<MarkEvent> — NodeStart / NodeEnd / EatToken / Trivia — never a tree
tree             CppTreeBuilder: events → rowan::GreenNode, via CppGreenNodeBuilder (flat element list,
                 node-edge correction for trivia, forward-parent re-nesting)
red layer        rowan::SyntaxNode<CppLanguage>: built on demand, parent pointers free
```

Numbers: the grammar is 16 763 lines (`types.rs` 6 158, `decls.rs` 5 410, `exprs.rs` 3 316, `stats.rs` 1 879), the parser
state and recovery 2 088 (`cpp_parser.rs`, plus `marker.rs` 302 and `parser_config.rs` 257), the lexer 1 220, the tree
layer 1 068, the kinds 1 600, the doc layer 1 200. **476 tests**, including a 7 355-line `gaps.rs` corpus and an
`invariants.rs` that asserts the event stream stays balanced.

Nothing above it can parse: the crate's job is to turn preprocessed text into a lossless tree, and it does that one
way — `CppParser::parse(text, config) -> CppSyntaxTree`. There is no incremental reparse anywhere in the workspace
and no typed-AST layer in use (`syntax/mod.rs` says the `node/` + `traits.rs` files are Lua-era and deliberately
unwired).

## 2. What it costs

### Throughput

```text
                                        MB/s      notes
lexing alone                            132–149   on 0.74 MB of code / 3.98 MB of std headers
whole parse, project sources            6.2–9.8   0.74 MB concatenated / 39 files
whole parse, libstdc++ headers          2.5       3.98 MB, 1 053 260 tokens
  of which: tree building               21–24      26 % of the parse on plain code, 12 % on std headers
  of which: the grammar                 —          74 % / 88 %
per token (plain code)                  ~0.6 µs
per token (std headers)                 ~1.5 µs
```

The important ratio is the last one: **template-dense code costs ~2.5× more per byte than plain code**, and that
difference is entirely in the grammar — speculation, `TypeNames` lookups, the `<`-is-it-a-template question. The tree
library's share is stable at 21–24 MB/s whatever the input.

### Where the grammar's time goes

`CPPLS_PARSER_STATS=1` prints a phase table (inclusive times, from `parser/phases.rs`) and a set of counts. On the
3.98 MB standard-header unit — 972 104 tokens, 688 159 nodes, 2 467 911 events — the parse is **1.56 s** and it splits:

```text
declaration              769 ms   27 306 calls    28.2 µs each     48 % of the parse
  specifiers             192 ms   64 052 calls    3.0 µs each     12 %   ← the largest single named item
  declarator              82 ms   43 524 calls    1.9 µs each      5 %
  expression              69 ms   49 399 calls    1.4 µs each      4 %
  template-arguments      32 ms   19 114 calls    1.7 µs each      2 %
  declarator-suffixes    0.1 ms      254 calls
tree building            187 ms   688 159 nodes                  12 %
lexing                    27 ms   978 289 tokens                  2 %
doc layer               10.6 ms    8 887 comment groups         0.7 %   (1.2 µs each)
statement              1901 ms   36 804 calls   — inflated: a file-scope declaration is parsed *through*
                                                `parse_stat`, and nested statements count for each enclosing one
```

Counts, from the same switch: 115 086 checkpoints (0.12 per token), 15 665 rollbacks, 38 411 type-name questions, 2 751
`>`-closing questions, and the doc layer at ~1 µs per comment group. **Per-file cost is flat** — the slowest ten files
of the corpus are 113–128 µs/KB against an average of 110 — so there is no pathological construct anywhere in
libstdc++, and the 2.5× over plain code is not one hot spot but the ordinary cost of the constructs std headers are
made of.

That last number is the important one for anyone hoping to "optimize the template path": **there is no single place to
win 2×**. The grammar's cost is spread over declaration machinery and per-token work; the best single named target is
`parse_decl_specifier_seq` at 12 % of the parse.

A translation unit is not what a reader feels, either: the analysis parses a **rendering** (the file with its macros
expanded, which for a header-heavy file is the whole closure). The parser's own dump tool records that case: *"on the
3.3 MB cooked `<vector>` stream the parse is about 0.8 s"* — 4 MB/s on the worst realistic input, and close to the
2.5 MB/s measured here on std headers.

### Weight

```text
                                  nodes    tokens   tree ÷ file
project source, 40 KB file         2 357     5 034      8.0×
project source, 0.74 MB aggregate  107 346   195 705      ~18×
std header, 0.29 MB                  —         —        23.6×
std headers, 3.98 MB aggregate     688 159  1 053 260     —
```

**A rowan tree is roughly 8–24× the text it describes**, and the reason is structural: `rowan 0.17` stores each token
as its own `ThinArc` allocation with the token's **bytes copied into it**, and each node as another allocation holding
its children. For 1 million tokens that is ~1 million allocations, ~4 MB of copied text, and ~16 bytes of allocator
granularity each — an order of magnitude more than the source file.

The console already knows this is the expensive half: `ParserConfig` carries a **skip-function-bodies mode** whose own
note measures **64.4 % of nodes and 61.9 % of tokens inside a `CompoundStat`** — a body is not something another file
can name, so the indexer does not build one. That is the parser's own biggest lever and it is already pulled where it
can be.

### Walking it

```text
red-tree walk (descendants)         42–67 ns/node
  a 107k-node tree                  4.5–7.2 ms
  a 688k-node tree                  42 ms
```

Every `descendants()` is an allocation per red node and a offset computation. The analysis layer walks trees
constantly (checks, semantic tokens, folding, definitions), so this is a real per-request cost — but it is `rowan`'s
cost model rather than a defect in this code.

### Accuracy, which is the number that matters most

```text
corpus                        files   with syntax errors   errors   not lossless
E:\EmmyLuaCodeStyle sources     114          1                 2          0
libstdc++ 15.1 headers          120          8                47          1
```

**One file in 114** of real project code has a syntax error, and **zero** losslessness violations there. On the
standard library — 8 files with errors out of 120, 47 errors in total — that is a good showing for a hand-written C++
grammar, and `gaps.rs` is the corpus that tracks the remaining shapes.

### Scaling — is anything quadratic?

```text
one construct repeated          MB/s at 1×    2×      4×      8×
int f(int a);                     3.08       3.10    3.00    2.89
template<class T> void g(...)     3.84       3.61    3.38    3.25
```

Mildly superlinear (−6 % and −15 % over 8× the size), which is cache and allocator behaviour rather than an algorithmic
term. The two quadratic defects the grammar has already had — the closed-marker scan and the annotation follower walk —
are fixed, and this is what "fixed" looks like.

### rowan's own interning, measured and not worth it

`ParserConfig` has carried a `rowan::NodeCache` since it was written and **nothing has ever passed one**, so identical
subtrees are never shared. Passing one:

```text
3.98 MB std headers, no cache     1.602 s   2.48 MB/s
3.98 MB std headers, NodeCache    1.626 s   2.45 MB/s
```

No gain — the lookup costs what the duplicate allocation saves at this scale. Worth recording as a *no*, so the next
reader does not spend the afternoon on it.

## 3. Is building on rowan reasonable?

**Yes, with the cost understood — and the cost is not what is holding the performance.**

What it buys, and this code uses all of it:

* **Lossless and immutable**, which is the whole contract above: a summary, a formatter rewrite and a rename all read
  ranges out of this tree, and 114/114 files round trip.
* **Cheap sharing.** A `GreenNode` is an `Arc`: a subtree can be cloned into a cache for nothing, which is what makes
  a per-file tree cache even conceivable (§6.1).
* **Red nodes with parents, on demand.** The analysis is written against `CppSyntaxNode` with `ancestors()`,
  `descendants()`, `parent()`; without a tree that answers "who is my parent" every rule would carry its own stack.
* **`SyntaxNodePtr`** for a position that survives a re-parse — the shape every LSP layer needs.

What it costs, measured above: the per-token allocation and copied text (8–24× the file), the red-node allocation per
walk (42–67 ns/node), and no arena to make either cheap.

The alternatives, honestly compared:

| option | gain | cost |
|---|---|---|
| custom green tree with **token ranges** instead of token text | removes the copied text and, with an arena, most of the per-token overhead — maybe 30–50 % of tree memory | replaces `GreenNode` with a bespoke type: the red layer, `SyntaxNodePtr` and every consumer's `CppSyntaxNode` walk must be rewritten; the code already tracks `SourceRange` per token in the builder and *throws it away* at `finish` |
| `tree-sitter` | incremental reparse, fast | a C dependency, a different error-recovery model, no parent pointers without a custom layer, and the grammar would be rewritten in its DSL |
| clang's AST over FFI | the real thing | not lossless, not rewritable, enormous, and the preprocessing would have to be clang's too |

The measurement that decides it: **tree building is 12–26 % of the parse**. Even an infinitely fast tree would cut the
parse by a quarter — while the grammar's template-heavy path is 88 % and costs 2.5× plain code. So: **keep rowan**, and
spend the effort where the parse actually goes. The one thing worth revisiting later, if tree *memory* becomes the
constraint rather than time, is the token-range builder — and the flat element list that already exists
(`CppGreenElement::Token { kind, range }`) is exactly the data such a tree needs.

## 4. The one correctness bug: a comment inside `extern "C" { }` is dropped

`crates/cpp_parser/tests/lossless.rs` has the reproducer, ignored and documented:

```cpp
extern "C" {
// one
int f(int a, int b);
// two                      ← this comment's bytes are consumed and never emitted
int g(int a, int b);
}
```

The tree's own dump shows how: an **empty `ErrorNode`** where the comment was.

```text
Syntax(CompoundStat)
  Syntax(DocComment)        ← the first one survived
  Syntax(Declaration)       ← f
  Token(Newline) Token(Whitespace)
  Syntax(ErrorNode)@126..126   ← zero width
  Syntax(Declaration)       ← g
```

`parse_linkage_block`'s recovery branch marks an `ErrorNode` and calls `bump` — and `bump` emits nothing when the token
at the cursor is **trivia**, which a comment is (`skip_trivia` skips comments; the doc layer re-emits them). So the
cursor is standing on trivia inside this one container, `bump` silently walks past it, and the comment's bytes are
gone. Everything after it is then shifted: `cxxabi.h` is **22 315 bytes in, 19 987 bytes out** (677 of 717 lines), and
the file still reports *zero* syntax errors.

That is the worst class of bug for a lossless tree — silent and position-shifting — and `extern "C"` is not exotic:
every C API header has one. The control test (`a_comment_between_two_declarations_survives`) passes for the same text
without the linkage block, so the machinery for comments is fine; the container is what is wrong.

Where to look, and how far the investigation got. The invariant being violated is "the cursor always stands on a token
`bump` can emit", and it is now **asserted** in `CppParser::bump` — the whole parser suite (476 tests) still passes
with the assert in place, so this shape is the only place it happens. With the assert in, the failing test names the
exact step:

```text
the cursor is on LineComment at 23, which `bump` cannot emit — every later offset is shifted
  at decls.rs:5786                       the linkage loop's "always advance" branch
```

and a trace of the loop says where the cursor came from:

```text
LINKAGE loop: cursor IntKeyword at 8   (before=8)     the first declaration parses
LINKAGE loop: cursor LineComment at 23 (before=23)    the previous iteration left it there
```

So the cursor is left on the trailing comment by `parse_declaration` — deliberately: its `;` reader stops **before** a
comment, so that a comment written below a declaration is not filed under that declaration.

## 4a. Resolved: the container emits the trivia, as the class body already did

The conclusion above ("the fix belongs where the cursor is set") was wrong, and the shape it rejected is the fix: **the
linkage block is the one declaration container that never emitted the trivia it inherits.** `parse_class_body_members`
does it at the top of every iteration, with a comment that says exactly why —

> `expect_semicolon` consumes a member's `;` and stops before a comment, so that a comment written below a member is not
> filed under it. What that leaves is the comment sitting at this loop's cursor, and a loop that went straight on to read
> a member would read the comment as rubble.

— and the linkage block's loop is the second of the two containers. The one-line fix is `p.emit_trivia_after_current_token()`
at the top of its body, before the `}` test, and with it:

* a comment between two declarations inside `extern "C" { … }` is emitted into the block, which encloses both and is the
  honest owner of the text between them;
* the zero-width `ErrorNode` the recovery branch used to produce is gone, so the recovery branch is no longer entered at a
  comment at all;
* `tests/lossless.rs::a_comment_between_two_declarations_in_a_linkage_block_survives` passes, un-ignored, with the shapes
  around it (a block comment, a comment before the closing brace, a comment after a definition, a declaration after the
  block).

The trace is removed with it. Nothing else changed: the `bump` invariant stays as the probe (`CPPLS_TRIVIA=1`), and the
whole parser suite — 476 tests — and `cpp_code_analysis`'s pass with the fix in place.

## 5. Code quality

### What is good, and unusual

* **The documentation is the design.** Every non-obvious rule carries the measurement that chose it (the `>`-analysis
  matching clang's `GreaterThanIsOperator` with a `bc<(1 > 2)>` example from `xutility`; the closed-marker journal with
  0.29 ms/KB vs 1.17 ms/KB; the winnt.h linkage-block disaster). This is rarer than it should be.
* **Recovery is structural, not best-effort.** `ParseResult` + `finish_marks_to`/`end_marks_to` means a failed rule
  cannot leak an open node, and `EventStreamAudit` makes imbalance testable. The `parse_cpp_unit` loop guarantees both
  termination and losslessness on any input.
* **Speculation is cheap and explicit.** `Checkpoint` truncates events and restores the cursor; the `>>`-split moved
  from the parser to the lexer after it was measured as O(k·n) — a zipper was built to fix it and then deleted when
  the split made it unnecessary. That kind of un-doing is a sign of a codebase that measures.
* **Only two `unsafe` blocks**, and 23 `unwrap`/`expect` in ~20k lines of src.
* **476 tests**, with a corpus per language level and a `gaps.rs` that pins known gaps as counts.

### What is risky, ranked

1. **The two `unsafe` transmutes are a latent-UB hazard** (`kind/mod.rs`). `try_from_raw` range-checks `raw <=
   LastVariant as u16` and then transmutes, which is sound **only while every variant's discriminant is contiguous from
   zero**. A future `enum CppSyntaxKind { …, Gap = 900 }` makes `900 as u16`-checked values transmute into invalid
   inhabitants — UB, in a parser that otherwise has none. Deriving `TryFrom<u16>` (or a macro-generated table) would
   remove the hazard and cost nothing at runtime.
2. **`Checkpoint` is a hand-maintained 10-field snapshot** (`cpp_parser.rs`). Five of those fields are state that lives
   *outside* the event stream — declaration type name (twice, `Box<str>`, cloned per checkpoint), whether the type was
   qualified, the closed-marks journal length, the error count, the terminator flag, the two bracket numbers — and each
   one is a documented near-miss: a field that is forgotten makes a speculative parse leak state into the reading that
   is kept. The docs are excellent; the *shape* invites the bug. A `struct Speculative` with one `save()`/`restore()`
   pair (and `Clone` where cheap) would make forgetting impossible.
3. **`closed_marks: HashMap<usize, bool>`** — the key is a dense event position and the value is a bit. A `Vec<bool>`
   indexed by event position, or a `Vec<u8>` with the journal, would be smaller and faster, and the journal already
   exists to make rollback O(changed).
4. **Two grammar files are 11 500 lines.** C++ is big, and splitting for its own sake is not a virtue — but `types.rs`
   (6 158) and `decls.rs` (5 410) are past the size where a reader can hold one file, and `mod decls` / `mod types`
   would cost nothing.
5. **Lua-era dead code on disk** (`syntax/node/`, `syntax/traits.rs`, ~2 900 lines) that nothing compiles into the
   crate. It is documented as "kept as a reference", which is fair, but it will drift silently and it is the kind of
   thing that gets edited by mistake. Either wire it up or delete it.
6. **`parse_with_events` clones the event stream** — a debug path, and worth a `#[doc(hidden)]`/feature gate so it is
   not reached for in production.
7. **`follower_memo: RefCell<HashMap<usize, bool>>`** on the hot path — a `RefCell` borrow per annotation, plus a
   hash lookup. The comment explains why it exists (a run of `M(a) M(b) …` was O(n²)); a small `Vec`-based memo or an
   `AssertUnwindSafe`-free `&Cell` array would be cheaper, but this is a micro-optimisation, not a defect.

### What I would do next, and what I would not

Ranked by (measured win) ÷ (risk):

1. ~~**Cache the `FileView` per (file, content hash, environment)**~~ — **done**, and the numbers are below. The
   analysis re-parsed a rendering on *every* `Session::view`, i.e. on every hover and every pull.
2. ~~**The grammar's template path**~~ — **tried, and the hypothesis was wrong.** There is no template-path hot spot:
   per-file cost is flat (§2), the phase table puts 50 % in `parse_declaration` and 12 % in its specifier sequence, and
   the counts say speculation is 0.12 checkpoints per token. What was measured and changed is in §5.3; the honest
   conclusion is that **a 2× needs several 10 % items**, starting with `parse_decl_specifier_seq` (12 %).
3. **Fix the linkage-block losslessness bug** (§4) — small, and it is a correctness bug in a construct every C header
   uses.
4. **`unsafe` removal** (§5.1) — zero risk to change, removes a UB hazard.
5. **Speculation state as one struct** (§5.2) — refactor with no behaviour change, removes a class of latent bug.

Not worth doing: replacing rowan (§3), `NodeCache` (§2, measured no gain), incremental reparse (a re-parse of a 200 KB
file is 30 ms; the win is in caching whole views, not in splicing them).

### What item 1 became, measured

A `FileView` cache in the session, keyed by `(path, the rendering *by object*, the file's identity, the language,
`cuda`, whether the buffer is open, the macro environment for a buffer view)` — see `Session::views` for why none of
those can serve a stale parse. Two more numbers came out of doing it:

```text
                                               before        after
11 KB file, a view                              1.5 ms       11 µs   (200 hits in 2.2 ms)
0.08 MB rendering (a 213 KB header), a view    15.4 ms      185 µs   (200 hits in 37 ms)
```

The *first* attempt still cost 145 µs a hit, and the phase timers said where: `CompilerConfig::for_file` (132 µs) —
it clones the configuration and looks the file up in the compile database, and it was being paid *after* the parse
became free. `language_for` cost the same again by itself (it re-does the lookup and re-parses `-std=`), so both are
now memoized per file, together, in `Session::config_for_file`.

Two things had to change for a session to hold a tree at all:

* **`FileView.tree` and `.scopes` are behind `Arc`s.** A view is cloned per request, and before this a clone copied
  the token stream — twelve bytes per token, megabytes for the units this exists for.
* **The cached half has no red root.** rowan's `SyntaxNode` is neither `Send` nor `Sync` and a session is shared
  across the server's threads, so the cache holds a `ParsedView` — everything but the root — and
  `FileView::from_parsed` rebuilds it per hit, which is one allocation.

### What item 2 became, measured

Item 2 said the template path was where a 2× lived. **It is not**, and finding that out is the result.

What was tried first was the marker bookkeeping, because `Marker::complete` asked a hash map three questions per node
and `close_mark` scanned the open stack:

```text
closed_marks: HashMap<usize, bool>  →  Vec<u8> indexed by event position
  close_mark      4 hash lookups + 2 linear scans per node → 3 loads, one byte each
  rollback        remove a map entry per position closed since → truncate the table
2.48 → 2.54–2.57 MB/s (3.98 MB of std headers, three runs after the change against a 2.48–2.52 baseline)
```

Real, reproducible, and **2.5 % — not 2×**. It is kept because it is strictly less work per node and makes a rollback a
truncation, but it is not what the review predicted.

Then the phase table (§2) answered where the time actually is, and the answer rules out the premise: **no hot spot**.
The parse is 1.56 s for 3.98 MB, split `declaration 50 % (specifier sequence 12 %, declarator 5 %, expression 5 %,
template arguments 2 %), tree 12 %, lexing 2 %`, and per-file cost is flat at 110 µs/KB with the slowest file at 128.
A speculative parser that re-reads 12 % of its tokens would show up as a file- or construct-shaped spike; there is
none.

What is left behind for the next round:

* `parser/phases.rs` — the phase timer, `CPPLS_PARSER_STATS=1`, free when off (one relaxed `OnceLock` load per
  instrumented call) and inclusive by design, so a parent's number contains its children's.
* `ParserCounts` — checkpoints, rollbacks, type questions, `>` questions, comment groups and their time.
* The measurement that matters: **`parse_decl_specifier_seq` is 12 % of the parse at 3.1 µs a call**, and the
  declaration machinery around it is another ~25 %. Those are where a 10 % win each is available; a 2× is not.

## 6. How to reproduce every number

```powershell
$env:CPPLS_PROJECT_ROOT = "E:\EmmyLuaCodeStyle"                                   # or a std-header tree
cargo test --release -p cpp_parser --test parse_cost -- --ignored --nocapture     # throughput, weight, accuracy
cargo test -p cpp_parser --test lossless                                          # the losslessness invariant
cargo test -p cpp_parser --test lossless -- --ignored                             # the linkage-block reproducer, red
```

And the phase table behind §2:

```powershell
$env:CPPLS_PROJECT_ROOT = "C:\…\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0\include\c++"
$env:CPPLS_PARSER_STATS = "1"
cargo test --release -p cpp_parser --test parse_cost -- --ignored --nocapture 2>&1 | Select-String "parser"
```

The std-header corpus used for the hard numbers:

```text
C:\Users\zc\Desktop\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0\include\c++
```
