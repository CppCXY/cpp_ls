Are we suppressing errors? Three layers, one real suppression

Asked directly, and the answer is not one thing. Measured with examples/diag_state.rs,
which runs the same path a client's request does (index, did_open, index, diagnose_file).

=== 1. The raw reading's errors: correctly NOT reported ===

  xutility   the file's own text has 39 parse error(s) that a raw reading would report
  xstring    3
  vector     4

None of these reaches a user now, and that is right rather than a suppression: an
unexpanded file is not a program, so a parse error in it is an error about text no compiler
reads. Reporting them would be the "inventing problems" failure this crate's check layer is
written against — and the cooked reading of every one of those files is CLEAN. This is the
3000-line payoff of the boundary change, and it is measurable:

  xstring    diagnostics: reading Cooked | 0 error(s), 0 note(s), 0 check(s), 0 unplaced
  vector     diagnostics: reading Cooked | 0 error(s), 0 note(s), 0 check(s), 0 unplaced

=== 2. unplaced: counted, logged, and NOT published -- a real suppression ===

  xutility   session.diagnostics answered NONE -- the client is shown an empty list

`handlers/diagnostic/mod.rs:60-68` handles errors whose range lands in another file's macro
body. Its own comment states the rule exactly:

  "**Said out loud rather than dropped.** A reading whose errors land in another file's macro
   bodies cannot show them here, and a client that shows an empty list must not be read as
   'this file is clean' when the parser had something to say and the answer is 'not here'."

What it actually does is `debug!` — a log line. The client sees nothing. So the comment
describes the intent and the code does the opposite: the number is counted, named, and then
kept from the only reader who could act on it. That is a suppression, and it is the exact
failure mode the comment warns about, one layer below where the comment is written.

=== 3. A file with no cooked reading: `null`, which is indistinguishable from clean ===

`Session::diagnostics` answers None for a file the index holds no cooked reading for, and
`document_diagnostic.rs:41` turns that into a null report. A client shows no diagnostics. It
has no way to tell "nothing is wrong" from "nothing has been read the way a compiler reads
it" — and the live case is a header that was never asked for, which is most of them.

  xutility   NO DIAGNOSTICS AT ALL, and its raw text has 39 parse errors

For an OPEN file this is recoverable, and it now is: `AnalysisState::prepare` queues the
cooked reading and (as of the wake fix) the pump is told to build it. For a file nobody
opened there is nothing to queue, and the answer stays empty for ever.

=== WHAT TO DO ABOUT 2 AND 3, in the order they are worth doing ===

2 first, because it is small and it is a stated intent that the code does not keep: publish
the unplaced count as a diagnostic (an Information or Warning with no range, or a range at the
top of the file) rather than only a log line. It is the one place where the analysis knows it
could not say everything and the user is the only one who can act on it.

3 next, and it is a protocol question rather than a code one: what a server says when it has
not read a file yet. The options are a diagnostic saying so, an empty report marked in some
way a client will re-ask for, or the existing `isIncomplete`-style arrangement where the
client is told to come back. This is the same decision the completion handler already makes
(`is_incomplete: found.items.is_empty() || ...`), and diagnostics have no equivalent yet.

=== WHAT IS NOT A SUPPRESSION, checked so it is not mistaken for one ===

  * an error whose offsets do not map gets `Range::default()` rather than being dropped, so
    the message still reaches the user (mod.rs:82-86);
  * the check layer reports only a definite Known::No, which is a deliberate silence with a
    documented rule rather than a swallowed error;
  * `unplaced` is counted, so the information exists -- it is only the publishing that fails.
