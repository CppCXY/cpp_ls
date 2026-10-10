Audit: is the current approach right? Correct, with one broken seam

Verdict: the DIRECTION is right and the evidence for it is strong; the IMPLEMENTATION is
incomplete in a way that is silent, and that is the thing to fix before anything else.

=== WHAT IS RIGHT, with the evidence ===

1. Raw text cannot carry meaning. Measured, examples/cooked_parse.rs:

     <xutility>  raw 84 error nodes / cooked 0   (77 files stitched, 1 095 871 bytes)
     <memory>    raw  8 / cooked 0
     <xstring>   raw 17 / cooked 0
     <utility>   raw 12 / cooked 0

   A tree built from unexpanded text is shaped like a syntax tree and means nothing. This is
   not a preference; it is a measurement, and every defect this session chased came out of it.

2. One authority. FileView no longer has a raw constructor, DiagnosticReading::Raw is
   deleted along with diagnostics' raw arm, and Session::tokens_of is the lexical reading
   whose TYPE has no tree and no scopes. The boundary is expressed in the type system rather
   than in a comment, which is the strongest form it can take.

3. The deferral is signalled, for completion. handlers/completion/mod.rs:191 is

     is_incomplete: found.items.is_empty() || session.pending() > 0 || found.truncated

   and its own note records why an EMPTY answer must never be final. So a reading that is not
   ready yet produces "come back", which is the contract the deferral needs.

4. The two rulers are explicit and there is one way between them. reading_offset_of /
   file_offset_of, documented on FileView. Three defects this session were one missing
   translation, and each was fixed by going through that pair rather than by special-casing.

=== WHAT IS WRONG, and it is the reason the handlers still fail ===

Session::view_of_the_file now returns None, and the completion handler opens with

    let view = session.view_of_the_file(&path)?;      handlers/completion/mod.rs:157

The `?` returns None from the HANDLER, which the transport reports as a null result — not as
an empty CompletionList with is_incomplete: true. So the deferral is broken at exactly the
seam that was supposed to carry it:

    the session says "not yet"        ->  the handler says "no answer at all"

For completion this is recoverable (a client re-asks on the next keystroke) but it is not
what the layer promises. For the handlers with no isIncomplete at all — references, rename,
definition, semantic_token, inlay_hint — "not ready" is indistinguishable from "nothing
found", and references/mod.rs:132 and :276 already record that the protocol gives them no way
to say "and there may be more".

That is the real remaining work, and it is not the coordinate translation: it is deciding
what each handler says when the reading is not ready. The options are not equal:

  * ask for the reading and RETRY (`want_cooked_reading` then `view` again in the same
    request) — the session can build it, and a request that waits is honest; the cost is the
    65-190 ms a cook of a header takes, paid once, by the request that needs it;
  * return an empty answer — dishonest wherever the protocol has no isIncomplete;
  * return null as today — the client cannot tell it from a crash.

The first is the one that matches the architecture: a reading is built on request, and a
request that needs one should cause one.

=== TWO THINGS THE BOUNDARY LEFT BEHIND ===

1. FileView::parse_with is still `pub` and still builds a tree AND scopes from the file's own
   tokens with only the closure's macro bodies supplied. It has ZERO callers — the audit
   found that and I deleted its two Session wrappers — but it is reachable, so the boundary is
   not closed: the constructor that should not exist is still there for a future caller to
   find. It should go the way of Session::view_of_the_file_with_macros.

2. FileView::parse is now a stub that answers None with a #[deprecated] note. That is
   defensible as a signpost, but it is also a public function whose name lies, and the
   project's own rule is that a function whose contract is "the answer is wrong" gets deleted
   rather than kept (the note on build_declarations says exactly this). Deleting it and
   keeping the explanation in the module header would be more consistent.

=== WHAT I CANNOT VERIFY FROM HERE ===

Whether a real client re-asks after a null or an empty answer. I have not exercised the LSP
transport end to end this session, and the answer decides how bad the seam above is per
handler. That is the next thing to measure rather than to reason about — the same rule this
session has applied throughout.

=== THE MEASURED STATE ===

  cpp_code_analysis lib        595 passed / 0 failed
  cpp_code_analysis completion  13 passed / 12 failed
  the 12 are behaviour assertions, and at least one of them (the comment case) is a real
  defect with a measured shape: a cursor in a region the rendering DELETED maps onto the next
  token, so a question about prose is answered about code.
