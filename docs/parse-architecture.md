# One file, three artifacts: how the parse should be split

The editor's report is the specification: an edit to a file should not reparse the whole **stitched** stream — the file's
text plus every include's text, macro-expanded, which for `#include <vector>` is tens of thousands of lines — when the
include region has not changed.

A real compiler does not do that either: it preprocesses, and the parser sees one stream. The stream is the *right*
artifact for the **first** read and the wrong one for the **second**, because an edit changes one region of it.

## The three artifacts

```text
the include region's tree      the stitched stream's prefix, up to the file's own first token
the file's own region's tree   the file's own text, expanded, with each include's tokens elided
the file's lex stream          the file's own text as tokens — no tree at all
```

* **The include region** is a function of the closure and of the compilation's configuration, both of which an edit to
  the file does not touch. It is already built once for a program read and cached by `SummaryKey` — the parse of it is
  the piece that is not reused.
* **The file's own region** is the only thing a keystroke changes. Its tree is what most queries about the file need.
* **The lex stream** answers the questions a keystroke actually asks before any parse finishes: colouring, inlay hints
  for what is plainly written, and the bracket/structure queries. It is the cheapest artifact and the one that exists
  first.

## What has to be true for this to work

1. **The parse is a function of a token range.** `cpp_parser` takes a text and returns a tree; it needs to take a
   **slice of the stitched stream's tokens** and return a tree of that slice, with the offsets it already uses. The
   stitcher (`summary::stitch`) already knows where each file's tokens start and end, in both the stream and the file.
2. **A tree of a slice is anchored.** A node's range is a stream offset; a query at a file offset has to be mapped
   through the file's spans — which the rendering already records (`RenderedSpan`).
3. **Declarations that straddle the boundary are handled.** A declaration can begin in the include region and end in
   the file (a class opened in a header, closed here — the shape MSVC's `<vector>` writes for `_STD_BEGIN`). The own
   region's parse must be told what the include region left open: the **scope stack** at the boundary, which the
   stitching walk already computes for the guard/brace accounting (`RenderedUnit::unbalanced`). Feeding that as the
   initial scope is how a slice's tree joins the whole.
4. **The index's facts are per region.** A unit's facts already carry a file and a range; a file's own facts come from
   its own region's tree, so an edit reindexes only those.

## Order of work

1. Make `cpp_parser` able to parse a **token range** of a text, not only the whole of it. This is the enabling
   refactor and it is testable on its own: a slice of the stitched stream must produce the same tree shape it produces
   today when the whole stream is parsed. **Done — see below.**
2. Teach the stitcher to hand the regions to the parser: the include prefix once, the own region per edit.
3. Rebuild the file's queries on the own region: colouring and inlay from the lex stream, the rest from the own tree
   plus the include prefix's scope stack.
4. Measure the two numbers the report is about — keystroke latency, and the time from opening a file to the first
   answer — with `perf-probe` marks, the same way the allocation work was measured.

Step 1 was where this started, and it was the only step that could be attempted before the others had a design.

## Step 1, as built

`CppParser::parse_tokens(text, &tokens[from..to], config)` — plus `parse_tokens_with_audit`, the twin
`parse_with_audit` has — parses a run of a longer text's tokens. The text must be the text those tokens came from,
because a green tree takes every token's text from it by range.

**Nothing in the grammar changed**, and that is the interesting part: the parser was already a function of a token
stream, because every token lookup goes through an index (`token_index`, `skip_trivia`, `peek_token_kind_at`) and every
text lookup goes through a token's own range (`self.text[token.range.start_offset..…]`). The refactor was therefore about
the *seams*: `with_text` was split into a lex and a `with_tokens`, and the tail of a parse (`parse_cpp_unit` → the audit →
the tree) became `run` + `finish_tree` so that both entry points differ in one thing only — where the stream came from.

Three properties are pinned in `crates/cpp_parser/tests/token_range.rs`, and they are the contract step 2 writes against:

* **A run of every token is the same parse.** The range entry point with the whole stream gives the whole text's tree.
* **A run of one top-level construct is that construct.** For every top-level child of the whole parse, parsing its own
  tokens alone gives the same shape — node kinds and significant tokens — as the child has inside the file.
* **Every run is lossless, however it is cut.** All windows of 1…21 tokens across a file, including cuts in the middle
  of a construct, reproduce exactly their own bytes and terminate. A cut is not promised a *clean* tree; it is promised
  an accounted-for one.

Two facts matter for step 2 and are stated rather than discovered later:

* **A tree of a run has node ranges relative to the run, and token ranges relative to the file.** A green tree is built
  from token lengths, so its root starts at zero however far into the text the run began; the anchor is
  `tree.get_tokens()[0].range.start_offset`, and every token in the tree keeps the range the lexer gave it. Error
  offsets are the lexer's too, so a diagnostic from a run maps through the file's line index with no arithmetic —
  but a *node* range needs the anchor added, which is the mapping item 2 above is about.
* **The grammar sees end-of-input after the run's last token**, because that is the truth about what it was given. A run
  must therefore end where a construct ends, which a region boundary is: the include region ends after a header's last
  top-level declaration. A run cut mid-construct parses as a truncated construct — a legitimate tree, and the reason
  the scope stack at the boundary (item 3) has to be fed in rather than guessed.

