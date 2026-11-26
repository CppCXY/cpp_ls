use std::sync::Arc;
use tokio::sync::Mutex;

use crate::context::{UpdateEvent, UpdateInbox, lsp_features::LspFeatures};

use super::{
    AnalysisState, DiagnosticService, NativeFileWatcher, client::ClientProxy,
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

    pub fn workspace_manager(&self) -> &Mutex<WorkspaceManager> {
        &self.inner.workspace_manager
    }

    pub fn status_bar(&self) -> &StatusBar {
        &self.inner.status_bar
    }

    pub fn lsp_features(&self) -> &LspFeatures {
        &self.inner.lsp_features
    }

    pub fn diagnostics(&self) -> &DiagnosticService {
        &self.inner.diagnostics
    }

    pub fn native_watcher(&self) -> &NativeFileWatcher {
        &self.inner.native_watcher
    }

    /// The last successful lens list for each document — see [`ServerContextInner::code_lenses`].
    pub fn code_lenses(
        &self,
    ) -> &std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, Vec<lsp_types::CodeLens>>>,
    > {
        &self.inner.code_lenses
    }

    /// The client's pending notifications, applied by one worker before request barriers complete.
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
    pub workspace_manager: Arc<Mutex<WorkspaceManager>>,
    pub status_bar: Arc<StatusBar>,
    pub lsp_features: Arc<LspFeatures>,
    /// The client's pending notifications. See [`ServerContextSnapshot::enqueue`].
    pub inbox: UpdateInbox,
    pub diagnostics: DiagnosticService,
    pub native_watcher: NativeFileWatcher,
    /// **The last lens list answered for each document.** A code lens is a count of references, and the count is
    /// computed from the file's summary — which an edit deliberately drops before the writer rebuilds it. Answering
    /// the drop as an empty list made every keystroke clear the lenses and the next refresh redraw them, which moves
    /// the lines below a lens and reads as the whole window jumping. Keeping the last answer lets the request be
    /// answered with the number it had until a new summary exists; the refresh after the index drains then updates it.
    pub code_lenses: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<String, Vec<lsp_types::CodeLens>>>,
    >,
}
