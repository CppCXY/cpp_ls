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

use crate::handlers;
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
    // **What the client says the file is** — the editor's language mode, which is the only statement about a header
    // nobody configured. See [`family_of`] and `Session::did_open_as`.
    let family = family_of(&params.text_document.language_id);
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
        handlers::start_analysis(context.clone()).await;
        handlers::register_files_watch(context.clone()).await;
    }

    apply_buffer_as(&context, path.clone(), text, family).await;
    // **An open is applied and caught up before the event is marked applied.** The first requests after `didOpen` are
    // entitled to the file's includes; the queue worker, not a request, pays for reading them.
    context.analysis().catch_up(&path).await;
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

    // **And the decorations for this file are cleared**, which is a message and not a consequence: a notification is
    // not an edit, so nothing on the client side knows the file it dimmed is gone (`vscode-clangd#600`: after a
    // restart the old ranges stayed and code that had become live was still greyed out).
    handlers::inactive_regions::clear(&context, uri.clone()).await;

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

/// **What the client's `languageId` says the file is**, reduced to a family.
///
/// The client is the only party that knows the *editor's* language mode, and it is what settles a **header** nobody
/// configured: `.h` is included by whichever language includes it, so the analysis refuses to guess a family from the
/// extension, and `DiagnosticCode::is_cpp_only`'s rule is "not told is not C" — which put C++ rules on a C project's
/// headers (measured on redis 8.x: **73 `cstyle-cast` findings, every one in a `.h`**; `docs/c-support.md` §8).
///
/// Anything that is not one of these maps to `None`, which changes nothing: naming a family is the client's answer
/// to a question the analysis cannot answer for itself.
///
/// **Both families are mapped, and C++ is the one that became load-bearing.** A header's language is now inferred
/// from the translation unit that includes it (`Session::config_for_file`), so a header a `.c` file includes is read
/// as C — and a reader who opens it as a C++ header is telling the session something the include graph cannot see.
/// Before the inference existed, "a client saying `cpp` changes nothing" was true.
fn family_of(language_id: &str) -> Option<cpp_parser::features::Family> {
    match language_id.to_ascii_lowercase().as_str() {
        "c" | "objective-c" => Some(cpp_parser::features::Family::C),
        "cpp" | "c++" | "objective-cpp" | "cuda-cpp" => Some(cpp_parser::features::Family::Cpp),
        _ => None,
    }
}

/// Tell the session what a path's text is now.
///
/// The caller schedules push diagnostics or a pull refresh after the update has been recorded.
async fn apply_buffer(context: &ServerContextSnapshot, path: PathBuf, text: String) {
    apply_buffer_as(context, path, text, None).await
}

/// **[`apply_buffer`], saying what the client says the file is** — `didOpen`'s `languageId`, and only `didOpen` has
/// one: a change or a save is about the text, not about which language the text is.
async fn apply_buffer_as(
    context: &ServerContextSnapshot,
    path: PathBuf,
    text: String,
    family: Option<cpp_parser::features::Family>,
) {
    let analysis: &AnalysisState = context.analysis();
    let for_session = path.clone();
    let applied = analysis
        .update_session("didOpen", move |session| {
            session.did_open_as(&for_session, &text, family);
        })
        .await;

    if applied.is_none() {
        // No workspace root has been named yet. The buffer is recorded above; the analysis will be told when a
        // session exists (`handlers::initialized` re-marks the open files), so this is a delay and not a loss.
        log::debug!("no analysis yet: the buffer is held until a workspace root is named");
        return;
    }

    // **The pump is woken before anything waits on it.** `did_open_as` dropped the summary and queued the file;
    // reading its includes is the background's work, and a request that needs the new reading waits (a read-only wait)
    // for this file to be ready rather than doing the work under the request's lock.
    analysis.wake();

    // The queued file is the pump's; parsing it is not part of this write-locked update.
}
