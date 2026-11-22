# The language server: what it answers, and what it does not

The reference for `cpp_ls` as a **server**: its capabilities, the boundaries a client can hit, and where each is enforced.
Configuration and analysis are in [`configuration.md`](configuration.md) and [`cpp-support.md`](cpp-support.md); the
protocol the analysis is asked over (for tests) is [`inspect.md`](inspect.md).

## What "supported" means here

Three different claims, and a capability in this server is only ever the first two:

| Claim | What it means |
| --- | --- |
| **Advertised** | the `initialize` response names it, so a compliant client will ask |
| **Implemented** | a request answered on the wire, end to end, with a test that makes the request through stdio |
| **Complete** | the answer is the whole truth about the project, not a slice of what the index has read so far |

The third is **not** claimed for anything. The analysis answers with `Known::Unknown` rather than a guess, and a query
that arrives before its closure is read answers `null` — a client that refreshes asks again, and one that cannot refresh
keeps what it had. `docs/code-analysis-review.md` and `docs/language-server-review.md` are the audits behind that
statement.

## The boundaries, as the review states them

Each one is a deliberate limit rather than a defect to be fixed later; a change to any of them is a product decision.

1. **One workspace root.** The session analyses the first root; additional folders are listed but not analysed, and
   `workspace/didChangeWorkspaceFolders` is not handled. (Enforced where the session is opened; see
   `WorkspaceManager::set_roots`.)
2. **No semantic-token range or delta provider.** Only the full-token request is implemented, so an editor that supports
   deltas still asks for everything.
3. **Full-document diagnostics only.** `textDocument/diagnostic` answers a whole file; workspace diagnostics are not
   implemented and `resultId` is not used, so a pull is never satisfied from a cached previous answer.
4. **The cold-start gap is repaired only where the client supports refresh.** `workspace/codeLens/refresh`,
   `workspace/semanticTokens/refresh`, `workspace/inlayHint/refresh` and `workspace/diagnostic/refresh` are sent when the
   index drains; a client that does not support them sees the earlier, shorter answer until the file changes.
5. **References and workspace symbols may be a partial answer.** The protocol has no `isIncomplete` for them; the index
   may still be reading when the request arrives, and the answer is what was read by then.
6. **Formatting is the project's formatter.** `textDocument/formatting` and `willSaveWaitUntil` answer with the configured
   formatter's edits; the LSP `FormattingOptions` (tab size, insert spaces) are not mapped onto it — the project's own
   configuration decides.
7. **`inactiveRegions` is clangd's extension**, not standard LSP. An editor that does not implement it gets the
   notification and ignores it; nothing else depends on it.
8. **No compiler-grade semantics.** Scope, type and overload answers are capped by what `cpp_code_analysis` proves: a
   name it cannot place answers `Unknown`, a type whose spelling does not name a class answers with the spelling, and a
   template instantiation is not evaluated. It is not a substitute for the compiler's own diagnostics.

## Where the wire and the code can disagree, and how that is prevented

Two kinds of drift have been measured, and both have a single authority now:

| The drift | The authority |
| --- | --- |
| A handler registered but not advertised (`willSaveWaitUntil`), or advertised but not answered | the `initialize` response, asserted end to end in `crates/cpp_ls/tests/suite/handshake.rs` |
| A file list the client is asked to watch that is not the file list the analysis reads | `cpp_code_analysis::source_extensions`, `cuda_source_extensions` and `CONFIGURATION_FILE_NAMES`, which the watcher's globs are built from |
