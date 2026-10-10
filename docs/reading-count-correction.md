Audit correction: "a view" was never one thing, and I collapsed it

The completion handler's own comment (handlers/completion/mod.rs:142-155) says, with
measurements, that a completion must query the file's own tokens and NOT the rendering:

  the rendering is built by DELETING the line breaks, so a file that reads as twenty lines
  becomes one. A cursor reader cannot survive that: measured on a live server, a plain
  identifier inside a function was read as a qualified name in scope `std`, because with
  everything on one line the recovery glued the `std::` of `std::string name;` to the
  `local_` being typed. The answer was `std`'s two hundred members while the variables in
  the function and the file's own globals were missing.

So there were THREE readings, each for a different question, and the audit's phrase "one
semantic authority" is right about MEANING and was wrong about POSITIONS:

  meaning        the rendering            Session::view              survives
  positions      the file's own tokens    Session::tokens_of         survives (lexical only)
  macros + cursor the file's tokens with its closure's macros   DELETED, and it is needed

The third is what the completion handler is asking for and not getting. My change made
view_of_the_file answer None, which for that handler is a REGRESSION rather than a deferral:
it cannot fall back to positions, because positions do not answer "which names are in scope".

WHAT IS ACTUALLY RIGHT ABOUT THE DELETION

The deleted constructor built a *scope tree* by parsing text whose macros were unexpanded, so
`_STD_BEGIN` was an identifier and declarations landed at file scope. That is the defect and
it stays deleted. The replacement must be a view of the file's own text WITH its macros
expanded in place -- file-aligned (so a cursor means what the client means) and semantically
sound (so a scope is where a compiler puts it). That is a different artifact from both the
rendering and the raw parse, and it is the one this handler has been asking for.

THE WAKE WAS A REAL FIX AND STAYS

analysis_state.rs:172 queued a cooked reading and nothing woke the consumer: wake() is called
by didOpen, didChange and watched-file events, and by no query path. So a file queued by a
request sat until the reader happened to type. Woken after the write now, with the note.

WHAT I DO NOT KNOW

Whether the completion handler can be migrated to the rendering at all, or whether the third
reading has to come back in a new shape. The measurement above says the rendering loses the
cursor; the reading_offset_of work this session fixed the OFFSET problem but not the
one-line problem, which is a property of the rendering's TEXT rather than of the mapping.
That is the next thing to measure: what the rendering does to a body's line structure and
what a cursor inside one resolves to.

This is the third time this session that I changed something with a documented measurement
against it, and the third time the code's own comment was the thing that caught it.
