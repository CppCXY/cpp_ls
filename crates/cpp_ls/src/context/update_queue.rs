//! # The update queue — one worker, one writer, in the order the client sent things
//!
//! Every notification that changes the analysis goes through here, and a **single** worker task takes them one at a
//! time:
//!
//! ```text
//! didOpen ─┐
//! didChange ├─▶ one mpsc queue ─▶ sync the buffer ─▶ session.did_open/did_change ─▶ schedule diagnostics
//! didClose ─┘
//! ```
//!
//! # Why one task rather than one per notification
//!
//! Because the order is the meaning. `didChange` then `didClose` is a file that was edited and closed; the same two
//! messages the other way round are a file that was closed (so the disk answers for it) and then edited. A runtime
//! that ran them concurrently could produce either, and the analysis holds one session with one `&mut` — which is
//! exactly the single-writer shape this queue gives it.
//!
//! # Why the handlers only enqueue
//!
//! So that the loop that reads from the client never waits for a parse. The notification handlers return as soon as
//! the message is queued, which keeps `$/cancelRequest` — and every other notification — answerable while a project
//! is being indexed. `docs/ls-architecture.md` §2 has the two paths end to end.

use std::sync::Arc;

use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams,
};
use tokio::sync::mpsc;

use crate::handlers;

use super::snapshot::{ServerContextInner, ServerContextSnapshot};

/// One thing the client said, waiting its turn.
///
/// The names drop the protocol's `did` prefix: this is a queue of *what happened* to a document, and the protocol's
/// own spelling is already on the parameter type inside each variant.
pub enum UpdateEvent {
    Opened(DidOpenTextDocumentParams),
    Changed(DidChangeTextDocumentParams),
    Saved(DidSaveTextDocumentParams),
    Closed(DidCloseTextDocumentParams),
    WatchedFilesChanged(DidChangeWatchedFilesParams),
}

pub fn spawn_update_queue(
    inner: Arc<ServerContextInner>,
    mut rx: mpsc::UnboundedReceiver<UpdateEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let snapshot = ServerContextSnapshot::new(inner.clone());
            match event {
                UpdateEvent::Opened(params) => {
                    handlers::process_did_open_text_document(snapshot, params).await;
                }
                UpdateEvent::Changed(params) => {
                    handlers::process_did_change_text_document(snapshot, params).await;
                }
                UpdateEvent::Saved(params) => {
                    handlers::process_did_save_text_document(snapshot, params).await;
                }
                UpdateEvent::Closed(params) => {
                    handlers::process_did_close_document(snapshot, params).await;
                }
                UpdateEvent::WatchedFilesChanged(params) => {
                    handlers::process_did_change_watched_files(snapshot, params).await;
                }
            }
        }
    });
}
