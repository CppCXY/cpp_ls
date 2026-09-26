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

use crate::context::{
    AnalysisState, DEFAULT_DIAGNOSTIC_INTERVAL, ServerContextSnapshot, UpdateEvent,
};
use crate::util::uri_to_file_path;

pub async fn on_did_open_text_document(
    context: ServerContextSnapshot,
    params: DidOpenTextDocumentParams,
) -> Option<()> {
    let _ = context.update_tx().send(UpdateEvent::Opened(params));
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
    let _ = context.update_tx().send(UpdateEvent::Changed(params));
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
    let _ = context.update_tx().send(UpdateEvent::Saved(params));
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
    let _ = context.update_tx().send(UpdateEvent::Closed(params));
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
        .update_session(|session| session.did_close(&path))
        .await;
    context.analysis().wake();

    // A file that is gone from disk has nothing left to diagnose, and the client's list for it has to be cleared by
    // hand: no later parse will ever publish an empty list for a file nobody can read.
    if !path.exists() {
        context
            .file_diagnostic()
            .clear_push_file_diagnostics(uri.clone());
    } else if !context.lsp_features().supports_pull_diagnostic() {
        // The path is the disk's now, and the text may differ from the buffer that was just dropped; the client's
        // diagnostics are about text that is no longer there.
        schedule_diagnostics(&context, path).await;
    }

    Some(())
}

/// Tell the session what a path's text is now, and schedule the diagnostics that follow.
async fn apply_buffer(context: &ServerContextSnapshot, path: PathBuf, text: String) {
    let analysis: &AnalysisState = context.analysis();
    let for_session = path.clone();
    let applied = analysis
        .update_session(move |session| session.did_open(&for_session, &text))
        .await;

    if applied.is_none() {
        // No workspace root has been named yet. The buffer is recorded above; the analysis will be told when a
        // session exists (`handlers::initialized` re-marks the open files), so this is a delay and not a loss.
        log::debug!("no analysis yet: the buffer is held until a workspace root is named");
        return;
    }

    // The edit *dropped* this file's summary and queued it (`Session::did_open`), so the index is stale until
    // something reads it again. That is the pump's job, and this is how it is told there is work.
    analysis.wake();

    schedule_diagnostics(context, path).await;
}

/// Schedule a file's diagnostics, unless the client pulls them instead.
async fn schedule_diagnostics(context: &ServerContextSnapshot, path: PathBuf) {
    if context.lsp_features().supports_pull_diagnostic() {
        return;
    }

    context
        .file_diagnostic()
        .add_diagnostic_task(path, DEFAULT_DIAGNOSTIC_INTERVAL)
        .await;
}
