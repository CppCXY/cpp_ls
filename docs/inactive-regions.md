# Greying out inactive code: what it would take

Written after the round that fixed two of the editor's five Lua reports (`PF_VAHID`'s expansion and the colour of
`ci->previous`) and explained the other three. **Nothing of this feature is implemented** — no protocol code, no span
plumbing, no client work. What is here is what the two layers already answer, what is missing between them, what it
would cost, and which of the open decisions are worth deciding *before* the first line is written.

Every number below was measured with `crates/cpp_toolchain/tests/inactive_regions_probe.rs`
(`cargo test --release -p cpp_toolchain --test inactive_regions_probe -- --ignored --nocapture`).

## 1. What the client needs, and why not the obvious shortcut

The feature is: text the current configuration does not compile is drawn dimmed, so a reader sees which side of an
`#ifdef` is live without asking for an answer.

clangd's protocol is the one worth copying — a dedicated server→client notification, not a decoration smuggled through
an existing channel:

```text
server → client   textDocument/inactiveRegions
                  { textDocument: { uri, version }, regions: Range[] }
server capability inactiveRegionsProvider: true
client capability textDocument.inactiveRegionsCapabilities.inactiveRegions : bool
```

It replaces an earlier attempt that carried the answer as **semantic highlighting** (the region's tokens sent as
comments). That attempt is recorded as a failure in the patch that replaced it — *"the experiment of highlighting the
inactive regions as comment is considered as a failure"* — and the reasoning is worth keeping: a token carries a
**type**, not a *state*, so the answer had to be minted as a pseudo-type, could not compose with the real one, and cost
a per-token payload for the whole region.

Three weaknesses of the clangd shape are known, and each is cheap to avoid:

* **No document version.** `sourcekit-lsp`'s issue #2222 names this first: a notification that does not say which text
  it describes can paint a live region after an edit. Ours carries `version` (an
  `OptionalVersionedTextDocumentIdentifier`), and the client drops a stale one.
* **No reason.** The range says *where*, never *why*. Optional here (see §5.5) — the preprocessor has the condition
  in hand but the span type does not carry it yet.
* **Nothing clears it.** vscode-clangd #600: after `clangd.restart` the old ranges stayed, and code that had become
  live was still dimmed. So the client must clear on `didClose`/restart, and the server must send an empty list on
  `didClose`.

## 2. What the server already has

The preprocessor already computes exactly this, and the comment where it records the spans says who for — *"the
`#endif` that closes it are part of the span a consumer greys out"*. In
`crates/cpp_toolchain/src/preprocess/cooked.rs`:

```rust
/// The spans of the regions that were **not** compiled, in source order.
pub inactive: Vec<SourceRange>,
```

filled while the walk decides branches:

```rust
// A directive belongs to the region it is written in: the `#if` that opens a branch nobody takes
// and the `#endif` that closes it are part of the span a consumer greys out, which is also what
// makes the pieces of one dead branch join into a single range.
if !was_live || !is_live(&regions) {
    merge_span(&mut out.inactive, spanned.range);
}
```

Four properties make it the right source rather than a starting point:

1. **File coordinates.** Every span is an offset into the text the walk cooked — the file's own text, not the
   rendering's — so an LSP range is a `LineIndex` lookup, not a mapping.
2. **One dead branch is one span.** `merge_span` joins a span whose end touches the previous one, so the blank lines,
   comments and nested `#if`s inside a dead branch are a single range.
3. **The directive lines are inside**, deliberately: that is what makes one dead branch one range instead of three
   fragments.
4. **The program path is right too, for a reason worth recording.** `UnitCook` splits a file's text at each `#include`
   and cooks the segments separately, which *looks* like it would cut a dead branch in half. It cannot: an `#include`
   inside a branch nobody takes is **not entered by the walk**, so it is not stitched, so the dead branch stays in one
   segment. A live branch spanning an include is split, but a live branch contributes no `inactive` span.

**And it must not be recomputed anywhere else.** A second implementation of "which branch is taken" — in the analysis
layer, from `FactGuard`s — is precisely the duplication this codebase keeps paying for; `CookedFile::macros`' doc says
the same thing about `#define`s. The spans are a *by-product of the renderer's own decision*, and the renderer is the
only layer that has the composed environment.

## 3. What is missing between the span and the notification

Four pieces of plumbing, none of which makes a decision:

1. **`RenderedCooked`** (`crates/cpp_toolchain/src/preprocess/cooked.rs:271`) gains `inactive: Vec<SourceRange>`,
   copied from `CookedStream.inactive` by `render()`. `Session::view` builds a cooked view out of this value, so
   anything that already holds one can answer.
2. **`CookedFile`** (`crates/cpp_code_analysis/src/index/project/mod.rs:102`) gains the same field, so a **warm start**
   answers without re-cooking. This one needs `summary_codec` and a `FORMAT_VERSION` bump — and it cannot be skipped in
   favour of memory only: a warm start hands back the *stored* rendering, and the spans cannot be derived from it. A
   rendering has no inactive text, but "no token" and "a comment" are indistinguishable in it.
3. **The program path**: `UnitCook::emit` (`crates/cpp_code_analysis/src/cache/summary.rs:2601`) collects each
   frame's `cook.inactive` (already in that file's coordinates), `RenderedUnit` carries them per file, and
   `index_unit_rendering` (`crates/cpp_code_analysis/src/index/mod.rs:597`) attributes them to each file's `CookedFile`
   in the same loop that attributes declarations.
4. **`Session::inactive_regions(&path) -> Option<Vec<SourceRange>>`**, through the rendering path that already falls
   back to the disk (`known_rendering_of`), plus the LSP side: the capability, the notification, a debounce, and the
   clear on close. The parts exist: `Client::send_notification`
   (`crates/cpp_ls/src/context/client.rs:44`), the `RegisterCapabilities` list (`crates/cpp_ls/src/handlers/mod.rs:162`),
   and `DiagnosticService::refresh_pull`'s `tokio::spawn` + `REFRESH_DELAY` pattern
   (`crates/cpp_ls/src/context/diagnostics.rs`).

Two details the protocol layer must get right:

* **Only send when the file's closure is ready** — the `is_locally_ready` gate diagnostics already use. Before that the
  answer is a guess about headers nobody has read, and a guess here is grey text over code that compiles.
* **The capability is not in the typed client capabilities.** `LspFeatures`
  (`crates/cpp_ls/src/context/lsp_features.rs`) keeps `lsp_types::ClientCapabilities`, which drops a non-standard field
  like `textDocument.inactiveRegionsCapabilities`. It has to be read from the raw `initialize` JSON (`experimental`) or
  from `initializationOptions` — the same route `clientInfo` already takes to decide whether a custom semantic token
  type is worth sending.

## 4. What it costs, measured

The probe cooks a directory of real files with an empty macro table — the "no configuration" case, so `#ifdef _WIN32`
branches read as dead — and prints the spans, the bytes of text they cover, and the JSON a notification would carry at
~90 bytes per range:

```text
Lua 5.5.0 src/          61 file(s)    77 region(s)    28 868 bytes of inactive text    ~113 B of JSON per file
MSVC 10.0.26100 ucrt/   61 file(s)   235 region(s)   610 683 bytes of inactive text    ~346 B of JSON per file

worst single files      corecrt.h   54 region(s), 91 549 bytes
                        tchar.h     15 region(s), 75 466 bytes
                        luaconf.h   15 region(s),  7 214 bytes
```

Three things follow:

* **The preprocessor costs nothing new.** The spans are recorded by the branch decision that was going to be made
  anyway; a file with no directives records nothing.
* **The payload is a rounding error.** A few hundred bytes per open file, sent when the file is cooked — not per token,
  not per keystroke.
* **The region count is what the client feels**, not the byte count: a worst-case header is ~50 decorations, which is
  nothing for `setDecorations` and would be **50 000 token modifiers** for the approach clangd abandoned.

## 5. Decisions this leaves open

1. **Are the directive lines dimmed?** The spans include `#if`/`#elif`/`#else`/`#endif` (§2.3). Dimming them says "this
   whole construct is not compiled", which is true; leaving them bright keeps the line a reader would click to change
   the configuration legible. A whole-line background decoration makes the first look intentional; cutting the
   directives out of the span costs the "one dead branch is one range" property. **Recommendation: keep the spans as
   they are, and let the client's whole-line option decide the look.**
2. **Keep or filter the semantic tokens inside?** Our semantic tokens are computed from the **written** view
   (`crates/cpp_ls/src/handlers/semantic_token/mod.rs:679`), so inactive text currently keeps its semantic colours.
   clangd removes those tokens so the editor's own TextMate colours show through and the dim composes with them. Both
   work: keeping them dims *accurate* colours, filtering gives the "dead code" look clangd chose. Filtering is one
   binary search per token against the spans. **Decide it once and write the reason in the handler** — a half-filtered
   look is the one outcome that is certainly wrong.
3. **What does "inactive" mean with one configuration?** Ours is a single configuration per project, so a region
   inactive for *another* target is dimmed. clangd and cpptools behave the same way, and the honest phrasing is "not
   compiled by the configuration this server uses". It belongs in the setting's description, not only in this file.
4. **Is there a fallback for clients that do not understand the notification?** Only `DiagnosticTag.Unnecessary` is
   standard enough to rely on — VS Code fades such diagnostics by itself. The cost is that it is the *diagnostic*
   channel: the Problems panel, the user's severity settings, and a contract with the checks. Zed converts clangd's
   notification into exactly these diagnostics **client-side**; that is the model to copy if a fallback is wanted, not
   a server that publishes them.
5. **Does a hover on dimmed code say so?** One line — "this region is excluded by the configuration" — is nearly free
   once the spans are cached, and it is the question the dimming raises. clangd does not answer it; cpptools disables
   IntelliSense in the region instead.

## 6. Alternatives, and why not

| Approach | Why not |
| --- | --- |
| Semantic token type/modifier `inactive` | clangd's own first attempt, later called a failure: a token states a *type*, the state does not compose, and the cost is one modifier per token — tens of thousands per large header. VS Code has no standard modifier→dim rendering, so every theme would have to cooperate. |
| `DiagnosticTag.Unnecessary` diagnostics from the server | Standard and client-free, but it is the diagnostic channel (§5.4), and "this was not compiled" is not a problem a reader should find in the Problems panel. Keep it as a *client-side* fallback. |
| `textDocument/publishDecorations` (rust-analyzer's shape) | The most expressive — a hover message, a background, a per-range style — and the least interoperable: no client implements it without being told to. |

## 7. Order of work

Each step is verifiable on its own, and the first one is a compiler comparison rather than code, because a wrong
decision here becomes *visible* wrongness — grey text over code that compiles.

1. **Ask the compiler.** Extend `crates/cpp_toolchain/tests/preprocessor_against_the_compiler.rs` with a fixture whose
   live branch spans an `#include`, and one with nested conditionals, and compare the spans against `g++ -E`'s output.
   The property in §2.4 is the one to pin.
2. **Span, memory only.** `RenderedCooked.inactive`, then `Session::inactive_regions`, then an `inspect inactive`
   subcommand to read the answer per file. No protocol yet.
3. **Persistence.** `CookedFile.inactive` in the codec, `FORMAT_VERSION` bumped, and a test that a warm start answers
   the same spans as a cold one — the `module-cache.md` failure mode (a missing field making a warm start silently
   different) is the precedent.
4. **The program path.** `RenderedUnit` per-file spans and the attribution in `index_unit_rendering`.
5. **The protocol.** Capability (raw JSON), notification with `version`, debounce, clear on close.
6. **The client.** vscode-clangd's semantics — opacity by default, a background option, a setting, and the clear on
   restart that #600 was about.

## 8. What could go wrong

* **A wrong region is now loud.** Every preprocessor bug in this project has so far surfaced as a name that does not
  resolve; this feature paints the answer on the screen. That is a net gain — and the reason step 1 exists.
* **Stale ranges.** Without `version`, an edit can leave a dimmed line of live code. With `version` and a client that
  drops stale notifications, it cannot.
* **Two configurations in one session.** Not possible today (one configuration per project); if that changes, the
  notification's meaning has to change with it.
* **The client's own bookkeeping.** 50 decorations a file is nothing; 50 leaked decorations a file across a restart is
  the bug vscode-clangd shipped once.

## References

* clangd's protocol extension, `Inactive regions` — <https://clangd.llvm.org/extensions>
* The failed semantic-highlighting attempt — <https://reviews.llvm.org/D67536>
* The dedicated-notification patch — <https://lists.llvm.org/pipermail/cfe-commits/Week-of-Mon-20230320/465999.html>
* The client that converts the ranges into `Unnecessary` diagnostics — <https://github.com/zed-industries/zed/blob/54114d24/crates/project/src/lsp_store/clangd_ext.rs>
* The version critique — <https://github.com/swiftlang/sourcekit-lsp/issues/2222>
* Highlights that outlive the server — <https://github.com/clangd/vscode-clangd/issues/600>
* Decorations without an opacity field (the C++ extension's request) — <https://github.com/microsoft/vscode/issues/43004>
