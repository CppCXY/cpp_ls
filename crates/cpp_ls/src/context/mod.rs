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
pub use client_id::{ClientId, get_client_id};
pub use diagnostics::DiagnosticService;
pub use lsp_features::LspFeatures;
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
    inner: Arc<ServerContextInner>,
}

impl ServerContext {
    pub fn new(
        conn: Arc<Connection>,
        client_capabilities: ClientCapabilities,
        client_id: ClientId,
    ) -> Self {
        let client = Arc::new(ClientProxy::new(conn));

        let analysis = Arc::new(AnalysisState::new());
        let lsp_features = Arc::new(LspFeatures::new(client_capabilities, client_id));
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
            inner,
        }
    }

    pub fn snapshot(&self) -> ServerContextSnapshot {
        ServerContextSnapshot::new(self.inner.clone())
    }

    pub fn send(&self, response: Response) {
        self.inner.client.send_response(response);
    }

    pub async fn task<T, F, Fut>(&self, req_id: RequestId, upto: u64, exec: F)
    where
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
        let mut started_changes = self.inner.inbox.started_changes();

        tokio::spawn(async move {
            let changed = async {
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
