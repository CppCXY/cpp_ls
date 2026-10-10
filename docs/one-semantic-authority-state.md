One semantic authority: the migration state, and the defect that blocks it

The boundary is in and the analysis tests pass; the handlers do not, and the reason is
one thing rather than many. Recorded because it is the third appearance of the same
defect class and the next round should start here rather than rediscover it.

WHAT IS DONE (lib: 595 passed / 0 failed)

- FileView::parse builds no tree and no scopes. The constructor that made "a view of a
  program" out of text a compiler never sees is gone.
- Session::view answers from a rendering or answers None. No fallback to the file's own
  tokens, in view / view_of_the_file / diagnostics alike.
- Session::tokens_of -> TokensOf: text, line index, the lexer's own tokens, and NO tree
  and NO scopes. The absence is the boundary.
- DiagnosticReading::Raw is deleted and diagnostics' raw arm with it. An untaken branch
  is not compiled, so an error inside one is about a program that does not exist --
  measured: 1 error as the file's text, 0 once cooked.
- IndexedRendering::rendered: a cook used to parse the rendering, keep the facts and
  throw the text away, which is why a file whose rendering had just been built was still
  answered None.
- Two real defects fixed on the way, both found by printing rather than by reading:
  * Session::view looked the rendering up under the CALLER's path spelling while the
    cache is keyed by the INDEX's (C:/... against C:\), so a miss was indistinguishable
    from "never cooked".
  * Session::view's ask went into macro_work, which the pump drained into the MACRO
    ENVIRONMENT builder -- nothing ever turned it into a cook, so cache stayed [] and
    every later view answered None for ever.

THE BLOCKER, stated exactly

A FileView's offsets are the RENDERING's; a client's cursor is an offset in the FILE.
While view answered from raw text those two coincided, so no handler had to know. Now
they differ by exactly the macro expansion, and every semantic handler is wrong until it
translates:

  crates/cpp_code_analysis/tests/completion.rs, a_comment_is_not_code:
    "Bad offset: range 0..6 offset 12"
    the fixture is 18 bytes of file text; its rendering is 6 bytes ("int x;")

The way between them already exists and is documented on FileView: reading_offset_of and
file_offset_of. The same pair bit twice before in this session -- macro_references on the
"FEATURE_ONLY" cursor (Unknown(UnparsableName)) and the disk-project test -- and both were
fixed by going through it. It is not a new problem; it is the one the boundary makes
unavoidable, and it is now the whole remaining cost of the change.

WHAT IS LEFT

1. Translate at every handler's entry: a file offset in, a reading offset for the query,
   and the answer's ranges back through file_offset_of on the way out. This is the same
   edit in each handler and it is mechanical once the first one is written.
2. Then re-run crates/cpp_code_analysis/tests/completion.rs (~20 tests) and the cpp_ls
   handlers, which fail for this reason and no other.
3. A file the index has never described needs want_cooked_reading (which queues READING
   as well as cooking) before view can serve it -- a cook is built out of a summary. view
   cannot do that itself: vfs.held borrows the session for the whole body, so the
   mutation cannot sit beside the lookup. Tried and reverted; the doc on Session::view
   records it.
4. The macro query has no entry point at all now: macro_references wants a name at an
   offset and gets one from a tree, but a macro is gone from a rendering. It wants the
   file's own tokens (tokens_of, which has them) plus the closure's macro facts.
