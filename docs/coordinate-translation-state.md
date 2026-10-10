The coordinate translation is in; the prefix lookup is the next thing

STATE: lib 595/595. completion 6/25. The completion failures are now ONE cause, and it
is no longer the coordinate translation.

WHAT WAS DONE

Session::name_completions and Session::completions now translate the cursor: a file
offset in through FileView::reading_offset_of, and a cursor that is not in the rendering
answers Unknown / an empty set rather than a list computed from the nearest position that
is. Put at that seam rather than in each handler, because every caller holds the same
pair -- a view and a cursor -- and a handler that forgot would get a wrong answer rather
than an error, which is what happened three times while this boundary went in.

VERIFIED WORKING, with the fixture's own numbers

SOURCE is 134 bytes and its rendering is 106 (the `#include "b.h"` line is replaced by
the header's 31-byte declaration, so offsets shift by -32):

  file offset 112 -> reading Some(80) | file 134 bytes, reading 106 bytes
  at the reading cursor: "local_variab"

112 is the cursor after `local` in `local_variable`, and reading offset 80 lands exactly
on that `local`. The translation is correct.

THE NEXT DEFECT, measured and not yet explained

  prefix "", 5 name(s)

The cursor is inside the token `local_variable` and 5 names are offered, so the scope
walk works -- but the **prefix is empty**. `name_completions_at` gets the prefix from
`crate::sema::resolve::name_position_at(root, offset)`, which reads a name position out
of the TREE. The tree is the rendering's and the offset is the rendering's, so the two
agree by construction; what is open is whether `name_position_at` answers for an offset
in the MIDDLE of a token the way it must have while the tree was the file's.

That is the next thing to print -- the answer of name_position_at at offset 80 -- rather
than to reason about: every wrong guess this session was made by reading code, and every
right answer came from a printed number.

WHY THE TESTS ARE STILL RED AND WHY THAT IS EXPECTED

19 of them assert `found.prefix` and the ranked order, so an empty prefix fails all of
them at once. None of them is failing for the coordinate reason any more: the panic
`Bad offset: range 0..6 offset 12` is gone from the suite.
