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
    let version = params.text_document.version;
    let text = params.text_document.text;
    let path = uri_to_file_path(&uri)?;

    // What the client has open is recorded whether or not a workspace is open yet: the buffer is the editor's, and
    // a workspace that arrives later re-applies it (`handlers::initialized`).
    let open_single_file_workspace = {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.open_file(uri.clone(), text.clone(), version);
        if workspace_manager.root().is_none() {
            if let Some(parent) = path.parent() {
                workspace_manager.set_roots(vec![parent.to_path_buf()]);
                true
            } else {
                log::warn!("cannot choose a workspace for {}", path.display());
                false
            }
        } else {
            false
        }
    };

    if open_single_file_workspace {
        crate::handlers::start_analysis(context.clone()).await;
        crate::handlers::register_files_watch(context.clone()).await;
    }

    apply_buffer(&context, path, text).await;
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;

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
    let version = params.text_document.version;
    let path = uri_to_file_path(&uri)?;

    let text = {
        let mut workspace_manager = context.workspace_manager().lock().await;
        match workspace_manager.change_file(&uri, version, &params.content_changes) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("cannot apply document change for {}: {error}", uri.as_str());
                return None;
            }
        }
    };

    apply_buffer(&context, path, text).await;
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;

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
        if let Err(error) = workspace_manager.replace_open_text(&uri, text.clone()) {
            log::warn!("cannot apply saved text for {}: {error}", uri.as_str());
            return None;
        }
    }

    apply_buffer(&context, path, text).await;
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;

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
    context
        .diagnostics()
        .close_document(&context, uri.clone())
        .await;

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
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;
    Some(())
}

/// Tell the session what a path's text is now.
///
/// The caller schedules push diagnostics or a pull refresh after the update has been recorded.
async fn apply_buffer(context: &ServerContextSnapshot, path: PathBuf, text: String) {
    let analysis: &AnalysisState = context.analysis();
    let for_session = path.clone();
    let applied = analysis
        .update_session("didOpen", move |session| {
            session.did_open(&for_session, &text);
        })
        .await;

    if applied.is_none() {
        // No workspace root has been named yet. The buffer is recorded above; the analysis will be told when a
        // session exists (`handlers::initialized` re-marks the open files), so this is a delay and not a loss.
        log::debug!("no analysis yet: the buffer is held until a workspace root is named");
        return;
    }

    // The queued file is the pump's; parsing it is not part of this write-locked update.
    analysis.wake();
}
