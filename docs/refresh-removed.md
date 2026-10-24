lsp: the server never asks for a diagnostic refresh — that request was the loop

A reader's log, at a full core, for as long as the window stayed open:

```
republishing open file diagnostics
index slice: prepared 0, committed 0, write lock held 0 ms, 1 to cook, 0 left
index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 9 left
index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 5 left
index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 1 left
index slice: prepared 0, committed 0, write lock held 0 ms, 1 to cook, 0 left
republishing open file diagnostics
…
```

Every line of it is one gesture: the pump drained -> the server sent `workspace/diagnostic/refresh` -> the client
pulled -> the pull found work -> the pump drained -> the server sent the refresh again. Nothing in that cycle is a
mistake on its own, and the cycle is why the flash survived every earlier fix: the answers were made stable, and the
*asking* was still aperiodic-but-endless.

So the request is gone, and there is no flag that brings it back. `workspace/diagnostic/refresh` cannot do anything
this server wants: it has no `workspace/diagnostic` report to redo (`workspace_diagnostics: false` in the
registration), and a pull client already asks on its own rhythm — after an edit, on a scroll, when a document is
opened. The server's job is to answer those asks, which it does from the index as it stands.

Removed with it:

```text
ClientProxy::refresh_workspace_diagnostics      the only sender
LspFeatures::supports_refresh_diagnostic        its capability check
republish_open_file_diagnostics                 the drain's call, and its log line
the watched-file handler's refresh              a watched file queues work and wakes the pump, nothing more
```

What a client is now told, in full: nothing it did not ask for. `tests/idle.rs` asserts both halves — an idle
session sends no `refresh` and no `publishDiagnostics`, and **one edit does not send
`workspace/diagnostic/refresh` either**; the hint and token refreshes are still real and stay inside that test's
budget.

`Session::want_cooked_reading` also grew the guard it was missing: "want" means "if it is not already read", as it
does in `want_the_closure_cooked`. Before it, every pull queued the file it was about, so a pull cost a render and a
parse of an open 4 000-line file — measured with the probe below, ten pulls in 2.8 s, `cook slice: asked 1, rendered
1` ten times.

Measured after (same files, same keystrokes): LuaSyntaxTree.cpp 209/169 ms with 8/10 findings, CodeFormat.cpp 1
finding — unchanged, as a fix that removes work must be. The probe that found all of this is
`crates/cpp_ls/tests/loop_probe.rs` (`#[ignore]`d, driven by `CPPLS_LOOP_ROOT`/`CPPLS_LOOP_FILE`), which starts the
real server over a real project and counts the log's lines by kind.

Still open, and measured: a pull asks for one or two *renderings* through `macro_work` and they are not found the
next time, so a pull still costs one render. That is a cache question, not a loop — the queue empties and the answers
hold — and it is the next number to look at rather than a reason to send a refresh.
