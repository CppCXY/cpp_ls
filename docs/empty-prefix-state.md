The empty prefix: `name_node_around` does not descend to the expression

Traced to a specific function and a specific reason, with every step printed. This is
where the completion suite's 19 failures stand, and it is NOT a coordinate problem: the
translation was verified working in the previous round (file 112 -> reading 80, and
offset 80 in the rendering is exactly the `local` of `local_variable`).

THE FOUR NUMBERS

1. The cursor: file 112 -> reading 80. File 134 bytes, rendering 106 (the `#include`
   line replaced by the header's declaration). Offset 80 is inside `local_variable`.

2. `name_position_at(&view.root, 80)` answers

     Some(NamePosition { scope: "", written: "", range: 80..80 })

   An empty `written`, which is the empty prefix. That is the fallback branch, taken when
   `name_node_around` finds no name node -- NOT the branch that walks a name's tokens.

3. The token at 80 is there:

     token at: Some(("Identifier", "local_variable"))

4. The ancestry that `name_node_around` walks, printed:

     TranslationUnit > Declaration > CompoundStat > ReturnStat "return local_variable + w.si"

   It enters `ReturnStat` and stops. And the subtree is NOT missing:

     ReturnStat children in the rendering: ["BinaryExpr"]

So the walk reaches `ReturnStat`, the `BinaryExpr` is a child of it, and the descent does
not take that child. `name_node_around` (resolve.rs:602) descends with

     .find(|element| contains_element(element, offset) || ends_at(element, offset))

and returns `found` -- still None -- when no child matches. The next step is to print what
that predicate answers for the `BinaryExpr` child at offset 80, because the answer decides
between `contains_element` being too strict and the `ReturnStat`'s child ranges being
offset from the reading's coordinates.

WHAT IS RULED OUT, each by a run rather than by reading

  the coordinate translation   verified correct (1 above)
  the raw text                 parses with `ReturnStat -> BinaryExpr -> IdentifierExpr`;
                               the same source with and without the `#include`, and with
                               the include moved after the struct, all give a BinaryExpr
  the grammar losing the node  the node is in the rendering's tree (4 above)
  an empty completion list     5 names are offered, so the scope walk and the index half
                               both work; only the prefix is empty

WHAT THE FIX LOOKS LIKE (hypothesis, to be settled by the print above)

`name_position_at` should also answer when the cursor is inside an **identifier token**
even if no name node encloses it: the token is there (3 above), it is an `Identifier`, and
the prefix is its text up to the cursor. That is the shape a client actually sends on every
keystroke -- the cursor is mid-word by definition -- and today it falls to the empty-prefix
branch. Doing that would make the prefix work regardless of why the descent stops, and the
descent can then be fixed on its own evidence.

19 tests assert `found.prefix` and the ranked order, so one cause fails them all at once;
they should turn green together.
