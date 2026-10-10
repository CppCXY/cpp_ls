How an error is discovered, and a case where it is discovered and then lost

The question was: if we never build a tree of the original file, how is an error found at all?
The mechanism answer is short. The follow-up measurement found a hole.

=== THE MECHANISM ===

Errors are found by parsing the **rendering** -- a real text, the one a compiler reads -- and
placed back into file coordinates through the span table. `index/mod.rs:414-432` is the whole
of it:

    for error in tree.get_errors() {
        let range = cpp_parser::source_range(error.range);
        let placed = rendered.reported_span(range)
            .or_else(|| rendered.reported_at(range.start_offset));
        match placed {
            Some(range) => diagnostics.push(CookedDiagnostic { range, message }),
            None => unplaced += 1,
        }
    }

So: `reported_span` for an error that covers tokens, `reported_at` for one that covers none,
and `unplaced` for one that lands nowhere in this file. The `#[derive]` on `RenderedSpan` says
why `reported` is the field used: it is *always* a position in this file — the token's own
range when the file wrote it, the outermost call site when a macro produced it.

=== THE MEASURED BEHAVIOUR, three shapes ===

  an error in the file's own tokens     "struct S { int a }"
      published: reading Cooked, 1 error(s), 0 unplaced
      at 17..18, and the text there is "}"          <-- correctly placed

  an error inside a macro body          #define DECLARE struct S { int a }  /  DECLARE;
      published: reading Cooked, 0 error(s), 0 unplaced    <-- NOTHING

  an error a token paste produces       #define CAT(a,b) a##b  /  struct S { int CAT(x, ;) };
      published: reading Cooked, 0 error(s), 0 unplaced    <-- NOTHING

=== THE CONTRADICTION ===

The loop above counts what it cannot place. For the second shape the parse of the rendering
must produce an error -- `struct S { int a }` with no `;` is the same text the first shape
reports on, and a macro body is spliced into the rendering -- and whatever the map does with
it, the count should be non-zero: either it is placed (then `errors` should be non-empty) or it
is not (then `unplaced` should be ≥ 1). **It is zero on both.** So an error is being dropped
between the parse and the answer, and the drop is silent by construction: nothing downstream
can tell "the parser had nothing to say" from "the parser said something that was thrown
away".

This is a FIFTH silent drop in the same family this session kept finding, and the family's
shape is always the same: a `match` whose `None` arm increments a counter that only a log
line reads. The other four were in the cook (`claim_cooking`, `materials_for`, `render_them`,
`view`), and they were found by printing each stage. This one is the same shape one layer
further down.

=== WHAT TO PRINT NEXT, and it is one line ===

Inside that loop, print every error's `range` and whether `reported_span`/`reported_at`
answered. That distinguishes three cases the current code cannot tell apart:

  * the parse of the rendering produced no error at all for the macro shape (then the answer
    is upstream, in the rendering: the text is not what I think it is);
  * it produced one and `reported_*` answered `Some` (then it is in `diagnostics` and
    something later drops it -- the cooked reading stored on the summary, or the lookup);
  * it produced one and both answered `None` (then `unplaced` should be 1 and the counter is
    not what the session reads -- which would make `Session::diagnostics` and
    `index_rendering` disagree about the same reading, the "two answers to one question"
    defect again).

=== WHY THIS MATTERS MORE THAN THE COUNTER ===

A user editing a file whose problem is inside a macro body currently gets **no diagnostic and
no indication that anything was not read**. That is the same failure the `unplaced` comment
was written against ("a client that shows an empty list must not be read as 'this file is
clean'"), except that here the number is not even counted, so the comment's remedy -- say it
out loud -- is not available either.
