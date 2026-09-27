use std::sync::Arc;
use tokio::sync::Mutex;

use crate::context::{UpdateEvent, UpdateInbox, lsp_features::LspFeatures};

use super::{
    AnalysisState, RequestManager, client::ClientProxy, diagnostic_service::DiagnosticService,
    status_bar::StatusBar, workspace_manager::WorkspaceManager,
};

#[derive(Clone)]
pub struct ServerContextSnapshot {
    inner: Arc<ServerContextInner>,
}

impl ServerContextSnapshot {
    pub fn new(inner: Arc<ServerContextInner>) -> Self {
        Self { inner }
    }

    pub fn analysis(&self) -> &AnalysisState {
        &self.inner.analysis
    }

    pub fn client(&self) -> &ClientProxy {
        &self.inner.client
    }

    pub fn file_diagnostic(&self) -> &DiagnosticService {
        &self.inner.file_diagnostic
    }

    pub fn workspace_manager(&self) -> &Mutex<WorkspaceManager> {
        &self.inner.workspace_manager
    }

    pub fn status_bar(&self) -> &StatusBar {
        &self.inner.status_bar
    }

    pub fn lsp_features(&self) -> &LspFeatures {
        &self.inner.lsp_features
    }

    pub fn request_manager(&self) -> &RequestManager {
        &self.inner.request_manager
    }

    /// **The client's pending notifications** — the queue a request applies before it reads the analysis.
    pub fn inbox(&self) -> &UpdateInbox {
        &self.inner.inbox
    }

    /// Queue one notification for the analysis, and wake whoever applies them.
    ///
    /// The only way a client's document event gets in: sending here is what gives it a sequence number, and the
    /// sequence number is what lets a request say "everything up to *here*" — see [`crate::context::update_queue`].
    pub async fn enqueue(&self, event: UpdateEvent) {
        self.inner.inbox.push(event).await;
    }
}

pub struct ServerContextInner {
    pub analysis: Arc<AnalysisState>,
    pub client: Arc<ClientProxy>,
    pub file_diagnostic: Arc<DiagnosticService>,
    pub workspace_manager: Arc<Mutex<WorkspaceManager>>,
    pub status_bar: Arc<StatusBar>,
    pub lsp_features: Arc<LspFeatures>,
    pub request_manager: Arc<RequestManager>,
    /// The client's pending notifications. See [`ServerContextSnapshot::enqueue`].
    pub inbox: UpdateInbox,
}
