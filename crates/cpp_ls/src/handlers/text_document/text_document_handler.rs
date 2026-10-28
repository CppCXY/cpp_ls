//! # `textDocument/*` — the document lifecycle
//!
//! Three kinds of message arrive here, and they are deliberately not symmetric:
//!
//! ```text
//! didOpen / didChange    the buffer is the text: the session is told, and a diagnostic pass is scheduled
//! didSave                nothing, unless the client sent the text (a save can carry an edit a change did not)
//! didClose               the disk is the text again: the buffer is dropped and the file re-read
//! ```
//!
//! The five `on_*` functions are what the dispatch table calls: they **enqueue** and return, so the loop reading
//! from the client is never held up by a parse. The `process_*` functions are what the update queue's single worker
//! calls, in the order the client sent things — see `crate::context::update_queue` for why that ordering is the
//! whole design.
//!
//! # Why a change is not a cheaper event than an open
//!
//! Because there is nothing to update in place. A change can declare a name, remove one, or add an `#include` that
//! changes what the file sees, so the summary is dropped and the file re-read — one parse of the file the user is
//! typing in, which no cache can answer because that text has never been seen (`Session::did_change` says the same
//! thing from the analysis side).

use std::path::PathBuf;

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams,
};

use crate::context::{AnalysisState, ServerContextSnapshot, UpdateEvent};
use crate::util::uri_to_file_path;

pub async fn on_did_open_text_document(
    context: ServerContextSnapshot,
    params: DidOpenTextDocumentParams,
) -> Option<()> {
    context.enqueue(UpdateEvent::Opened(params)).await;
    Some(())
}

pub async fn process_did_open_text_document(
    context: ServerContextSnapshot,
    params: DidOpenTextDocumentParams,
) -> Option<()> {
    let uri = params.text_document.uri;
    let text = params.text_document.text;
    let path = uri_to_file_path(&uri)?;

    // What the client has open is recorded whether or not a workspace is open yet: the buffer is the editor's, and
    // a workspace that arrives later re-applies it (`handlers::initialized`).
    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.sync_open_file(uri.clone(), text.clone());
    }

    apply_buffer(&context, path, text).await;

    Some(())
}

pub async fn on_did_change_text_document(
    context: ServerContextSnapshot,
    params: DidChangeTextDocumentParams,
) -> Option<()> {
    // Only enqueue: the worker reads these in order, and the loop that reads from the client must not wait here.
    context.enqueue(UpdateEvent::Changed(params)).await;
    Some(())
}

pub async fn process_did_change_text_document(
    context: ServerContextSnapshot,
    params: DidChangeTextDocumentParams,
) -> Option<()> {
    let uri = params.text_document.uri;
    // Full sync (`TextDocumentSyncKind::FULL` is what this server advertises), so the last change is the text. A
    // client that sent incremental changes anyway would need their ranges applied here, which is the one thing
    // advertising FULL is for: it does not have to.
    let text = params.content_changes.last()?.text.clone();
    let path = uri_to_file_path(&uri)?;

    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.sync_open_file(uri.clone(), text.clone());
    }

    apply_buffer(&context, path, text).await;

    Some(())
}

pub async fn on_did_save_text_document(
    context: ServerContextSnapshot,
    params: DidSaveTextDocumentParams,
) -> Option<()> {
    context.enqueue(UpdateEvent::Saved(params)).await;
    Some(())
}

/// A save is a statement that the buffer and the disk now agree.
///
/// Which is no work *unless* the client also sent the text: a save can carry an edit that no `didChange` reported
/// (`TextDocumentSyncSaveOptions::include_text` asks for it, and this server asks for `false` — a client that sends
/// it anyway is answered rather than ignored).
pub async fn process_did_save_text_document(
    context: ServerContextSnapshot,
    params: DidSaveTextDocumentParams,
) -> Option<()> {
    let text = params.text?;
    let uri = params.text_document.uri;
    // **No version to carry.** A save's params have no `version` field — the document's is the last `didChange`'s,
    // and naming it here would be guessing at which change the text belongs to.
    let path = uri_to_file_path(&uri)?;

    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.sync_open_file(uri, text.clone());
    }

    apply_buffer(&context, path, text).await;

    Some(())
}

pub async fn on_did_close_document(
    context: ServerContextSnapshot,
    params: DidCloseTextDocumentParams,
) -> Option<()> {
    context.enqueue(UpdateEvent::Closed(params)).await;
    Some(())
}

pub async fn process_did_close_document(
    context: ServerContextSnapshot,
    params: DidCloseTextDocumentParams,
) -> Option<()> {
    let uri = params.text_document.uri;

    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.close_open_file(&uri);
    }

    let Some(path) = uri_to_file_path(&uri) else {
        return Some(());
    };

    // The analysis is told the buffer is gone, which means the disk answers for this path again — and the file goes
    // to the *back* of the queue, because the user has stopped looking at it (`Session::did_close`).
    context
        .analysis()
        .update_session("didClose", |session| session.did_close(&path))
        .await;
    context.analysis().wake();

    // **Nothing is published, and nothing has to be.** Diagnostics are pulled: a client asks about the file it is
    // showing, and a closed file is one it is not showing — so there is no list of ours to clear, which is what the
    // `clear_push_file_diagnostics` branch here used to do by hand. The disk answers for this path again
    // (`Session::did_close`), and the next pull is answered from it.
    Some(())
}

/// Tell the session what a path's text is now.
///
/// **The diagnostics follow from the client's own next request**, which is the whole of the pull design: an edit is
/// recorded here, and the answer is computed when it is asked for. There is no schedule and no delay, and therefore
/// no window in which the client holds an answer about text that is gone.
async fn apply_buffer(context: &ServerContextSnapshot, path: PathBuf, text: String) {
    let analysis: &AnalysisState = context.analysis();
    let for_session = path.clone();
    let applied = analysis
        .update_session("didOpen", move |session| {
            session.did_open(&for_session, &text);
            // **The edit is read before this handler returns.** `did_open` drops the file's summary and queues it,
            // so until something reads it again the index describes the text the user just replaced — and the very
            // next request is a completion or a hover **about that text**. Leaving it to the pump is a race the
            // client cannot see: measured with `tests/handshake.rs`'s
            // `a_completion_sees_a_change_to_an_included_header`, a completion asked in the same breath as the
            // keystroke was answered from the old summary often enough that the test — which asserts the exact
            // opposite — failed in one full run in three while passing alone.
            //
            // `advance(1)` and not more: it takes the file at the head of the queue, which the `did_open` above put
            // there at `Priority::Open`, and reads **it** — a wave needs `left > 1`, so this is one parse and one
            // insert, the same work the pump's next slice would do. Its includes stay queued for the pump, so the
            // write lock is held for one file (measured 2-6 ms on `EmmyLuaCodeStyle`), well inside the budget.
            session.advance(1);
        })
        .await;

    if applied.is_none() {
        // No workspace root has been named yet. The buffer is recorded above; the analysis will be told when a
        // session exists (`handlers::initialized` re-marks the open files), so this is a delay and not a loss.
        log::debug!("no analysis yet: the buffer is held until a workspace root is named");
        return;
    }

    // Anything the read above discovered — the includes it queued — is the pump's, and this is how it is told.
    analysis.wake();
}
