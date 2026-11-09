use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use lsp_types::{Diagnostic, PublishDiagnosticsParams, Uri};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{CancelSource, RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::handlers::diagnostic::diagnose_file;
use crate::util::uri_to_file_path;

const DEFAULT_CHANGE_DELAY_MS: u64 = 300;
const REFRESH_DELAY: Duration = Duration::from_millis(200);

struct Pending {
    id: u64,
    cancel: CancellationToken,
}

pub struct DiagnosticService {
    pending: Mutex<HashMap<Uri, Pending>>,
    next_id: AtomicU64,
    change_delay_ms: AtomicU64,
    refresh_scheduled: AtomicBool,
    shutdown: CancellationToken,
}

impl Default for DiagnosticService {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            change_delay_ms: AtomicU64::new(DEFAULT_CHANGE_DELAY_MS),
            refresh_scheduled: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        }
    }
}

impl DiagnosticService {
    pub fn set_change_delay(&self, delay: Option<u64>) {
        self.change_delay_ms
            .store(delay.unwrap_or(DEFAULT_CHANGE_DELAY_MS), Ordering::Release);
    }

    pub async fn schedule_open_files(&self, context: ServerContextSnapshot) {
        if context.lsp_features().supports_pull_diagnostic() || self.shutdown.is_cancelled() {
            return;
        }
        let open = context
            .workspace_manager()
            .lock()
            .await
            .open_document_versions();
        for (uri, version) in open {
            self.schedule(context.clone(), uri, version).await;
        }
    }

    async fn schedule(&self, context: ServerContextSnapshot, uri: Uri, version: i32) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        {
            let mut pending = self.pending.lock().await;
            if let Some(previous) = pending.insert(
                uri.clone(),
                Pending {
                    id,
                    cancel: cancel.clone(),
                },
            ) {
                previous.cancel.cancel();
            }
        }

        tokio::spawn(async move {
            loop {
                let outcome = compute(&context, &uri, cancel.clone()).await;
                if !context
                    .diagnostics()
                    .finish(&context, &uri, version, id, outcome)
                    .await
                {
                    break;
                }
            }
        });
    }

    async fn finish(
        &self,
        context: &ServerContextSnapshot,
        uri: &Uri,
        version: i32,
        id: u64,
        outcome: RequestOutcome<Vec<Diagnostic>>,
    ) -> bool {
        let workspace = context.workspace_manager().lock().await;
        let mut pending = self.pending.lock().await;
        let Some(current) = pending.get(uri) else {
            return false;
        };
        if current.id != id {
            return false;
        }
        let current_version = workspace.open_version(uri) == Some(version);
        if !current.cancel.is_cancelled() && current_version {
            let published = context
                .inbox()
                .when_caught_up(|| match outcome {
                    RequestOutcome::Ready(items) => context.client().send_notification(
                        "textDocument/publishDiagnostics",
                        PublishDiagnosticsParams::new(uri.clone(), items, Some(version)),
                    ),
                    RequestOutcome::Missing => {
                        log::warn!("diagnostics are not ready for {}", uri.as_str());
                    }
                    RequestOutcome::Failed { message, .. } => {
                        log::warn!("cannot publish diagnostics for {}: {message}", uri.as_str());
                    }
                    RequestOutcome::Cancelled(_) => {}
                })
                .await;
            if published.is_none() {
                return true;
            }
        }
        pending.remove(uri);
        false
    }

    pub async fn close_document(&self, context: &ServerContextSnapshot, uri: Uri) {
        if let Some(pending) = self.pending.lock().await.remove(&uri) {
            pending.cancel.cancel();
        }
        if !context.lsp_features().supports_pull_diagnostic() {
            context.client().send_notification(
                "textDocument/publishDiagnostics",
                PublishDiagnosticsParams::new(uri, Vec::new(), None),
            );
        }
    }

    pub fn refresh_pull(&self, context: ServerContextSnapshot) {
        if !context.lsp_features().supports_pull_diagnostic()
            || !context.lsp_features().supports_refresh_diagnostic()
            || self.shutdown.is_cancelled()
            || self.refresh_scheduled.swap(true, Ordering::AcqRel)
        {
            return;
        }
        tokio::spawn(async move {
            tokio::select! {
                _ = context.diagnostics().shutdown.cancelled() => {}
                _ = tokio::time::sleep(REFRESH_DELAY) => {
                    context.diagnostics().refresh_scheduled.store(false, Ordering::Release);
                    context.client().refresh_diagnostics();
                }
            }
        });
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        for (_, pending) in self.pending.lock().await.drain() {
            pending.cancel.cancel();
        }
    }
}

async fn compute(
    context: &ServerContextSnapshot,
    uri: &Uri,
    cancel: CancellationToken,
) -> RequestOutcome<Vec<Diagnostic>> {
    let delay = context
        .diagnostics()
        .change_delay_ms
        .load(Ordering::Acquire);
    let Some(deadline) = tokio::time::Instant::now().checked_add(Duration::from_millis(delay))
    else {
        return RequestOutcome::Failed {
            code: lsp_server::ErrorCode::InvalidParams,
            message: format!("diagnostic debounce {delay} ms exceeds the supported timer range"),
        };
    };
    tokio::select! {
        _ = cancel.cancelled() => return RequestOutcome::Cancelled(CancelSource::Client),
        _ = tokio::time::sleep_until(deadline) => {}
    }

    let upto = context.inbox().queued();
    tokio::select! {
        _ = cancel.cancelled() => return RequestOutcome::Cancelled(CancelSource::Client),
        _ = context.inbox().wait_until_applied(upto) => {}
    }
    let Some(path) = uri_to_file_path(uri) else {
        return RequestOutcome::Failed {
            code: lsp_server::ErrorCode::InvalidParams,
            message: "diagnostics require a file URI".to_string(),
        };
    };
    let prepared = tokio::select! {
        _ = cancel.cancelled() => return RequestOutcome::Cancelled(CancelSource::Client),
        prepared = context.analysis().prepare_for_a_diagnostic(&path) => prepared,
    };
    if !prepared {
        return RequestOutcome::Missing;
    }
    snapshot_query(context.analysis(), cancel, move |session| {
        diagnose_file(session, &path).map(|(_, items)| items)
    })
    .await
}
