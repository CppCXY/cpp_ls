//! **`textDocument/inactiveRegions`** — the regions this configuration does not compile, sent so an editor can grey
//! them out.
//!
//! clangd's dedicated notification is the shape (`docs/inactive-regions.md` §1): a server→client message carrying
//! ranges, not a decoration smuggled through semantic tokens — a token carries a *type*, not a *state*, and clangd's
//! own experiment of minting a pseudo-type for it is recorded as a failure. The three weaknesses clangd's shape has
//! are each answered here:
//!
//! * **`version` travels**, so a client can drop a notification that describes text it no longer holds
//!   (`sourcekit-lsp#2222`);
//! * **nothing is painted when the answer is a guess** — [`cpp_code_analysis::InactiveRegions::is_exact`] — and an
//!   empty list is sent instead, which clears whatever was painted rather than leaving a stale region. This is the
//!   gate `docs/inactive-regions.md` §8 asks for, and it is the one failure a reader sees immediately: grey over code
//!   that compiles;
//! * **`didClose` clears** — a notification list is not an edit, so nothing else tells a client the file it dimmed is
//!   gone (`vscode-clangd#600`).

use cpp_parser::LineIndex;
use lsp_types::{Position, Range, Uri};
use serde_json::json;

use crate::context::ServerContextSnapshot;
use crate::util::{position_in, uri_to_file_path};

/// clangd's extension: the method a client listens on once it has seen `inactiveRegionsProvider`.
pub const METHOD: &str = "textDocument/inactiveRegions";

/// **Send the regions of every open document**, after the index has drained.
///
/// Called from the pump's idle loop beside the semantic-token refresh: the answer becomes available *without* the
/// document changing (the first cook of a header, a batch that read a macro's body), and a client that heard nothing
/// would keep the file undimmed forever. A file nobody has cooked yet is skipped — its request stays queued and a
/// later pass answers — which is why this is safe to call on every drain.
pub async fn publish_all(context: &ServerContextSnapshot) {
    if !context.lsp_features().supports_inactive_regions() {
        return;
    }
    let documents = context
        .workspace_manager()
        .lock()
        .await
        .open_document_versions();
    let mut asked = false;
    for (uri, version) in documents {
        asked |= publish(context, uri, Some(version));
    }
    // **A file with no rendering was asked for, and the ask must wake the pump.** The query queues the cook
    // (`Session::inactive_regions` asks when the rendering is unknown), and this runs *inside* the pump's idle
    // branch — after the drain that would have read it. Without the wake the ask sits in the queue until the next
    // edit, which is the one thing this feature must not require: a file opened and left alone would never be dimmed.
    if asked {
        context.analysis().wake();
    }
}

/// **Clear the regions of a document the client just closed.**
pub async fn clear(context: &ServerContextSnapshot, uri: Uri) {
    if !context.lsp_features().supports_inactive_regions() {
        return;
    }
    send(context, uri, None, Vec::new());
}

/// **The regions of one document**, as the answer stands now.
///
/// `None` sends nothing: the file has no rendering yet, and "no news" is the honest message — the alternative is an
/// empty list, which would tell a client that a file with a thousand dead lines has none.
fn publish(context: &ServerContextSnapshot, uri: Uri, version: Option<i32>) -> bool {
    let Some(path) = uri_to_file_path(&uri) else {
        return false;
    };
    let answer = context
        .analysis()
        .with_snapshot(|session| session.inactive_regions(&path))
        .flatten();
    let Some(answer) = answer else {
        return true;
    };
    // **A guess is not painted.** The spans are still real — a branch was dropped — but the configuration may not be
    // the one the user builds with (`assumed_undefined` counts branches decided by C's undefined-is-zero rule), and
    // the client is told "nothing" instead of told a lie.
    if !answer.is_exact() {
        send(context, uri, version, Vec::new());
        return false;
    }
    let text = context
        .analysis()
        .with_snapshot(|session| session.text(&path))
        .flatten();
    let Some(text) = text else {
        return false;
    };
    let regions = ranges_of(&text, &answer.spans);
    send(context, uri, version, regions);
    false
}

fn send(context: &ServerContextSnapshot, uri: Uri, version: Option<i32>, regions: Vec<Range>) {
    let document = match version {
        Some(version) => json!({ "uri": uri, "version": version }),
        None => json!({ "uri": uri }),
    };
    context.client().send_notification(
        METHOD,
        json!({ "textDocument": document, "regions": regions }),
    );
}

/// The file's spans as the protocol's ranges.
///
/// A span the file cannot place is dropped rather than clamped to byte zero: the spans come from the same text the
/// line index is built from, so an unplaceable one is a defect, and pointing it at the top of the file would put a
/// grey band over the first line of every file that ever hit it.
fn ranges_of(text: &str, spans: &[cpp_parser::SourceRange]) -> Vec<Range> {
    let index = LineIndex::parse(text);
    spans
        .iter()
        .filter_map(|span| {
            let start = position_in(text, &index, span.start_offset)?;
            let end = position_in(text, &index, span.end_offset()).unwrap_or(start);
            Some(Range::new(start, end))
        })
        .collect()
}

/// A position is the protocol's line and column; this exists so the module above reads as ranges rather than as
/// offsets, and so a future change of unit has one place to land.
#[allow(dead_code)]
fn at(text: &str, index: &LineIndex, offset: usize) -> Option<Position> {
    position_in(text, index, offset)
}
