mod analysis_state;
mod client;
mod client_id;
mod diagnostics;
mod lsp_features;
mod native_watcher;
mod query_runner;
mod snapshot;
mod status_bar;
mod update_queue;
mod workspace_manager;
mod workspace_state;
pub use analysis_state::AnalysisState;
pub use client::ClientProxy;
pub use client_id::{ClientId, the_effective_client_id};
pub use diagnostics::DiagnosticService;
pub use lsp_features::LspFeatures;
pub(crate) use lsp_features::wants_inactive_regions;
use lsp_server::{Connection, ErrorCode, RequestId, Response};
use lsp_types::ClientCapabilities;
pub use native_watcher::NativeFileWatcher;
pub use query_runner::{CancelSource, RequestOutcome, snapshot_query};
pub use snapshot::ServerContextSnapshot;
pub use status_bar::ProgressTask;
pub use status_bar::StatusBar;
use std::{collections::HashMap, future::Future, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
pub use update_queue::{UpdateEvent, UpdateInbox, catch_up_upto, spawn_update_queue};
pub use workspace_manager::WorkspaceManager;

use crate::context::snapshot::ServerContextInner;
use crate::util::catch_unwind;

pub struct ServerContext {
    cancellations: Arc<Mutex<HashMap<RequestId, CancellationToken>>>,
    /// **The request each `(method, document)` pair is currently waiting on** — see [`ServerContext::supersede`].
    slots: Arc<Mutex<HashMap<(String, String), RequestId>>>,
    inner: Arc<ServerContextInner>,
}

impl ServerContext {
    pub fn new(
        conn: Arc<Connection>,
        client_capabilities: ClientCapabilities,
        client_id: ClientId,
        initialization_options: Option<&serde_json::Value>,
    ) -> Self {
        let client = Arc::new(ClientProxy::new(conn));

        let analysis = Arc::new(AnalysisState::new());
        let lsp_features = Arc::new(LspFeatures::new(
            client_capabilities,
            client_id,
            initialization_options,
        ));
        let status_bar = Arc::new(StatusBar::new(
            client.clone(),
            lsp_features.supports_work_done_progress(),
        ));
        let workspace_manager = Arc::new(Mutex::new(WorkspaceManager::new()));

        let inner = Arc::new(ServerContextInner {
            analysis,
            client,
            workspace_manager,
            status_bar,
            lsp_features,
            inbox: UpdateInbox::new(),
            diagnostics: DiagnosticService::default(),
            native_watcher: NativeFileWatcher::default(),
        });
        spawn_update_queue(inner.clone());
        ServerContext {
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(Mutex::new(HashMap::new())),
            inner,
        }
    }

    /// **A newer question about the same document makes the older one pointless.**
    ///
    /// The client sends a completion (a hover, a signature help) after **every** keystroke, and it keeps only the
    /// answer to the newest one. Computing the older ones is work nobody reads — measured with an editor's request
    /// mix, a superseded completion ran for **170 ms** and produced a list the client dropped, while the request the
    /// client was waiting for queued behind it.
    ///
    /// So the older request in the slot is cancelled **before it computes**: the handler checks its token after the
    /// barrier and returns without running (see [`ServerContext::task`]). That is the difference between "do not
    /// cancel the request the client is waiting for" (the previous round) and "do not compute the answers the client
    /// has already asked over" (this one): the newest request in a slot always runs, the older ones never do.
    pub async fn supersede(&self, method: &str, uri: Option<&str>, id: &RequestId) {
        let Some(uri) = uri else {
            return;
        };
        let key = (method.to_string(), uri.to_string());
        let older = self.slots.lock().await.insert(key, id.clone());
        let Some(older) = older else {
            return;
        };
        if older == *id {
            return;
        }
        let cancellations = self.cancellations.lock().await;
        if let Some(token) = cancellations.get(&older) {
            token.cancel();
        }
    }

    pub fn snapshot(&self) -> ServerContextSnapshot {
        ServerContextSnapshot::new(self.inner.clone())
    }

    pub fn send(&self, response: Response) {
        self.inner.client.send_response(response);
    }

    /// **Run one request, and answer it unless the client cancels — or unless it is a refresh the edit already made
    /// pointless.**
    ///
    /// `upto` is the stream position the request recorded on arrival; the handler waits for it before doing its
    /// work ([`crate::context::catch_up_upto`]), so the answer is about text that includes everything the client had
    /// sent when it asked.
    ///
    /// # `cancel_on_a_change`: two kinds of question, and the measurement that separates them
    ///
    /// A client sends **six requests per keystroke** — completion, semantic tokens, inlay hints, code lenses, symbols
    /// and a pull diagnostic — and all six used to be cancelled with `ContentModified` (`-32801`) as soon as the next
    /// keystroke started, because the barrier takes 300–500 ms and a reader types every 80 ms. Measured through the
    /// protocol with that mix: **6 of 7 requests per keystroke** answered `-32801`, and the completion popup never
    /// appeared at all. Once the cancellation was removed for everything, the same mix answered — but the refreshable
    /// five queued ahead of completion and it took **3 s**.
    ///
    /// So the policy is per method ([`crate::handlers::a_newer_change_cancels`]): the **lists a client asks again for
    /// after every edit** are cancelled when the text has moved on (answering one is work nobody reads, and the next
    /// keystroke asks again); **interactive questions** (completion, hover, signature help, definition) are answered
    /// from whatever the barrier applied — the rule clangd states for completion, *"it just uses whichever is
    /// immediately available"* — and the client decides whether it still wants the answer (`$/cancelRequest`).
    ///
    /// A request of either kind is also superseded by a **newer request of the same method for the same document**,
    /// which is the client saying it wants a different answer — see [`ServerContext::supersede`].
    pub async fn task<T, F, Fut>(
        &self,
        req_id: RequestId,
        upto: u64,
        cancel_on_a_change: bool,
        slot: Option<(String, String)>,
        exec: F,
    ) where
        T: serde::Serialize + Send + 'static,
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = RequestOutcome<T>> + Send + 'static,
    {
        let cancel_token = CancellationToken::new();

        {
            let mut cancellations = self.cancellations.lock().await;
            cancellations.insert(req_id.clone(), cancel_token.clone());
        }

        let client = self.inner.client.clone();
        let cancellations = self.cancellations.clone();
        let slots = self.slots.clone();
        let mut started_changes = self.inner.inbox.started_changes();

        tokio::spawn(async move {
            let changed = async {
                if !cancel_on_a_change {
                    // A future that never resolves: the branch below is inert for this request.
                    std::future::pending::<()>().await;
                }
                loop {
                    if *started_changes.borrow_and_update() > upto {
                        break;
                    }
                    started_changes
                        .changed()
                        .await
                        .expect("the update inbox remains alive while a request runs");
                }
            };
            let res = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    Ok(RequestOutcome::Cancelled(query_runner::CancelSource::Client))
                }
                _ = changed => {
                    cancel_token.cancel();
                    Ok(RequestOutcome::Cancelled(query_runner::CancelSource::ContentModified))
                }
                result = catch_unwind(exec(cancel_token.clone())) => result,
            };

            let response = match res {
                Ok(RequestOutcome::Ready(value)) => Response::new_ok(req_id.clone(), value),
                Ok(RequestOutcome::Missing) => {
                    Response::new_ok(req_id.clone(), serde_json::Value::Null)
                }
                Ok(RequestOutcome::Failed { code, message }) => {
                    Response::new_err(req_id.clone(), code as i32, message)
                }
                Ok(RequestOutcome::Cancelled(query_runner::CancelSource::Client)) => {
                    Response::new_err(
                        req_id.clone(),
                        ErrorCode::RequestCanceled as i32,
                        "cancel".to_string(),
                    )
                }
                Ok(RequestOutcome::Cancelled(query_runner::CancelSource::ContentModified)) => {
                    Response::new_err(
                        req_id.clone(),
                        ErrorCode::ContentModified as i32,
                        "document changed while the request was running".to_string(),
                    )
                }
                Err(_) => {
                    log::error!("request {req_id:?} panicked");
                    Response::new_err(
                        req_id.clone(),
                        ErrorCode::InternalError as i32,
                        "internal error".to_string(),
                    )
                }
            };
            client.send_response(response);

            let mut cancellations = cancellations.lock().await;
            cancellations.remove(&req_id);
            drop(cancellations);
            // **The slot is released when the answer is sent** — and only if it still names this request: one that
            // replaced it owns the slot, and clearing it here would let a stale request look current.
            if let Some(key) = slot {
                let mut slots = slots.lock().await;
                if slots.get(&key) == Some(&req_id) {
                    slots.remove(&key);
                }
            }
        });
    }

    pub async fn cancel(&self, req_id: RequestId) {
        let cancellations = self.cancellations.lock().await;
        if let Some(cancel_token) = cancellations.get(&req_id) {
            cancel_token.cancel();
        }
    }

    /// Everything the workspace holds goes away: the pending diagnostics, the pending reloads, and the watcher
    /// registration the client was given.
    ///
    /// The analysis session itself is left open, because a client that says goodbye and then keeps talking (a
    /// restart in place) does not want its project re-read; a session is replaced by the next `initialized`.
    pub async fn close(&self) {
        self.inner.diagnostics.shutdown().await;
        let mut workspace_manager = self.inner.workspace_manager.lock().await;
        workspace_manager.shutdown().await;
        drop(workspace_manager);
        self.inner.native_watcher.stop().await;
    }

    pub async fn send_response(&self, response: Response) {
        self.inner.client.on_response(response).await;
    }
}

#[cfg(test)]
mod tests;
