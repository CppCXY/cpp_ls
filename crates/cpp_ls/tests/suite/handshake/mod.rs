//! An end-to-end test over the server's own stdio: the handshake, one edit, and the answers this server gives.
//!
//! Everything below `server/` is unit-tested — the analysis engine has a thousand tests of its own — and none of
//! that proves the *shell* works: that a client can say `initialize`, get capabilities back, open a file, and
//! receive a diagnostic, a definition and a hover over the wire. That is what this test is for, and it is the only
//! place in the crate that speaks the protocol as a **client** does.
//!
//! ```text
//! initialize ──▶ capabilities          definition ──▶ the declaration's file and range
//! initialized ─▶ the workspace opens   hover ──────▶ markdown about the name under the cursor
//! didOpen ────▶ the buffer is the text textDocument/diagnostic ──▶ the file's report
//! ```
//!
//! # Why diagnostics are **pulled** and not awaited
//!
//! The server does not publish diagnostics. `textDocument/diagnostic` is asked, and `Server::pull_diagnostics` is
//! how these tests ask it — the same request a client makes, so every diagnostic assertion here exercises the path
//! a user's editor takes. There used to be a push half beside it, and a client that declared
//! `textDocument.diagnostic` got that instead; two channels for one answer is how a screen ends up holding an
//! answer about text that is gone.
//!
//! # Why the definition is asked for more than once
//!
//! Because the index is **lazy on purpose**: `didOpen` queues the file, the background loop reads it and follows
//! its includes, and a query that arrives in between legitimately answers "not declared here yet" — the analysis
//! says so rather than guessing (`Session::pending` is how a caller tells that apart from a real absence). A test
//! that asked once would be asserting about the scheduler, so this one asks until it gets an answer, with a
//! deadline. A wrong answer is a wrong answer whenever it arrives; a slow one is not a failure.
//!
//! # Layout
//!
//! The protocol helpers are in [`support`], the source fixtures in [`fixtures`], and the tests are grouped by the
//! part of the protocol they exercise:
//!
//! ```text
//! diagnostics  pull/push reports, edits, and findings about deleted text
//! completion   completion lists, resolve, member access and the edits they carry
//! hover        types, members, documentation, macros and format strings
//! navigation   definitions, references, rename, symbols, hierarchies, selection
//! features     semantic tokens, inlay hints, signatures, folding, outlines, code lenses, formatting,
//!              and the cooked-index questions that feed completion
//! projects     watches, reloads, modules, CUDA, Ascend and the CLI
//! inactive     textDocument/inactiveRegions
//! ```
//!
//! Each file imports `super::support::*` and `super::fixtures::*`; a new test should do the same rather than creating
//! another `Project` or `Server` wrapper.

mod completion;
mod diagnostics;
mod features;
mod fixtures;
mod hover;
mod inactive;
mod navigation;
mod projects;
mod support;
