# Greying out inactive code: what it took, and what is left

**The feature is implemented** — server and VS Code client — in the five commits listed in §7. This note is kept as the
design record: what the two layers already answered, what was missing between them, what it cost, and which decisions
were made *before* the first line was written. The measurements in §4 are the ones the plan was priced with; the ones
that decided the gate are in §8.

Every number below was measured with `crates/cpp_toolchain/tests/inactive_regions_probe.rs`
(`cargo test --release -p cpp_toolchain --test inactive_regions_probe -- --ignored --nocapture`).

**What is not done:** the program path (§7 step 4 — a file whose reading comes from a whole-program walk answers from
the per-file cook instead, which is the same configuration and therefore the same spans), and the hover line of §5.5.

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

## 5. Decisions this leaves open — and how they were decided

1. **Are the directive lines dimmed?** The spans include `#if`/`#elif`/`#else`/`#endif` (§2.3). Dimming them says "this
   whole construct is not compiled", which is true; leaving them bright keeps the line a reader would click to change
   the configuration legible. **Decided: keep them** — the spans go out as the preprocessor recorded them, so one dead
   branch is one range, and the client may draw whole-line backgrounds if it wants that look.
2. **Keep or filter the semantic tokens inside?** Our semantic tokens are computed from the **written** view
   (`crates/cpp_ls/src/handlers/semantic_token/mod.rs:679`), so inactive text keeps its semantic colours today.
   clangd removes those tokens so the editor's own TextMate colours show through and the dim composes with them.
   **Decided: keep them.** They are accurate for the text that is there, dimming composes with a colour, filtering is
   one binary search per token plus a failure mode, and a reader editing a dead branch still benefits from the types
   the tokens name. If the look turns out confusing, the filter is the next step and the reason is recorded here.
3. **What does "inactive" mean with one configuration?** Ours is a single configuration per project. **Decided: the
   setting says it** — "the current configuration" — and, more important, the answer is only painted when the
   configuration actually knew: see the `assumed_undefined` gate in §8. A region the server guessed about is not greyed
   at all.
4. **Is there a fallback for clients that do not understand the notification?** Only `DiagnosticTag.Unnecessary` is
   standard enough to rely on. **Decided: no server-side fallback** — a client that does not ask for the notification
   gets nothing, which is the honest state of a feature it never heard of; Zed's client-side conversion is the model if
   one is ever wanted.
5. **Does a hover on dimmed code say so?** One line — "this region is excluded by the configuration" — is nearly free
   once the spans are cached, and it is the question the dimming raises. **Not done yet**; the spans are now one query
   away (`Session::inactive_regions`), so this is the next small piece.

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
## 7. What was built

| Step | Where | Commit |
| --- | --- | --- |
| 1. The compiler comparison | `crates/cpp_toolchain/tests/preprocessor_against_the_compiler.rs` — markers outside the spans are the markers `g++ -E` keeps; a dead branch whose `#include` names a header that does not exist is one span covering the include line | `d48d82b9` |
| 2. The spans travel | `RenderedCooked::inactive` → the cooked record (`COOKED_VERSION` 2 → 3) → `Session::inactive_regions` → `inspect inactive` | `965ca2d5` |
| 3. The gate | `RenderedCooked::assumed_undefined` (`COOKED_VERSION` 3 → 4) → `InactiveRegions::is_exact`; a branch decided by C's undefined-is-zero rule is answered but **never painted** | `7f4ac636` |
| 5. The protocol | `crates/cpp_ls/src/handlers/inactive_regions` — `inactiveRegionsProvider` written into the `initialize` result by hand, opt-in read from `experimental`/`initializationOptions`, `version` in the params, sent when a cook commits, empty list on `didClose` | `1975764b` |
| 6. The client | `E:\vscode-cppls` — `initializationOptions.inactiveRegions`, the notification listener, one opacity decoration, drop-on-stale-version, clear on close/restart/stop, `cppls.inactiveRegions.enabled` | `ade1869` |

Two things the plan got wrong and the implementation corrected:

* **Step 4 (the program path) was not needed for the feature.** A file's own cook uses the same configuration the unit
  walk would, so the per-file reading answers the same spans; the program path is only a *cheaper* way to get them for
  files already read as a unit, and it stays owed for latency rather than for correctness.
* **The idle drain is the wrong place to send from.** A slice ends with `continue`, so the iteration that cooked never
  reaches the drain's refresh block; the notification goes out right after `commit_them` instead, which is also the
  first moment the spans exist.

## 8. What could go wrong — and what was measured

* **A wrong region is now loud.** The gate is `assumed_undefined`: with an empty macro table the probe's 235 regions
  over the CRT headers are an *upper bound* of what a project without a configuration would see, and every one of
  those branches was decided by C's undefined-is-zero rule. Zero means the configuration knew; non-zero sends an empty
  list, so the failure mode is "no grey" rather than "grey over live code". The compiler comparison in step 1 is the
  other half: the spans the cook reports are the branches `g++ -E` drops.
* **Stale ranges.** The params carry `version`; the client drops a notification older than the document it is looking
  at (`describes_the_text`). Tested in `test/unit/inactiveRegions.test.mjs`.
* **Two configurations in one session.** Not possible today (one configuration per project); if that changes, the
  notification's meaning has to change with it.
* **The client's own bookkeeping.** One decoration type per extension, ranges keyed by document URI, cleared on
  `didClose`, on the document closing in the editor, on restart and on stop — the vscode-clangd #600 bug is the exact
  state this ordering exists to prevent.
* **The cost.** A notification per cooked open file: ~90 bytes per range, 1–54 ranges per file in the probe, sent when
  a cook commits and never per keystroke. No semantic token is added, removed or filtered.

## 9. Measured on a real project — and why most files answer "not paintable"

`inspect inactive` against `E:\EmmyLuaCodeStyle` (108 project files, MSVC 14.35 headers resolved):

```text
CodeFormatServer/src/LSP/LSP.cpp   0 region(s)                assumed_undefined 0   → paintable (nothing to grey)
3rd/nlohmann_json/.../json.hpp    17 region(s), 3 371 bytes   assumed_undefined 17  → not paintable
3rd/lua-5.4.3/src/luaconf.h       18 region(s), 9 387 bytes   assumed_undefined 19  → not paintable
```

The gate is working, and it is the feature's real limitation: **a project without a compile database mostly gets no
grey**, because the conditions of a portable header (`#if LUA_32BITS`, `#if JSON_HAS_...`) are names nobody defined —
C's rule makes them `0`, and the branch that follows is a *guess about the configuration*. Painting it would be exactly
the wrong-region failure §8 names. `LSP.cpp` is paintable because it has no conditional of its own; a *file's* answer
is exact when every branch in it was decided from a definition, not when the project has a database.

What would raise the paintable fraction, in order of value: a compile database (the defines arrive with the compile
command), `[compile].args` in `.cppls.toml` for projects without one, and the preprocessor's own `-D` seeds. None of
that is part of this feature — it is configuration quality, and the gate is how the feature refuses to paper over it.

## References

* clangd's protocol extension, `Inactive regions` — <https://clangd.llvm.org/extensions>
* The failed semantic-highlighting attempt — <https://reviews.llvm.org/D67536>
* The dedicated-notification patch — <https://lists.llvm.org/pipermail/cfe-commits/Week-of-Mon-20230320/465999.html>
* The client that converts the ranges into `Unnecessary` diagnostics — <https://github.com/zed-industries/zed/blob/54114d24/crates/project/src/lsp_store/clangd_ext.rs>
* The version critique — <https://github.com/swiftlang/sourcekit-lsp/issues/2222>
* Highlights that outlive the server — <https://github.com/clangd/vscode-clangd/issues/600>
* Decorations without an opacity field (the C++ extension's request) — <https://github.com/microsoft/vscode/issues/43004>
