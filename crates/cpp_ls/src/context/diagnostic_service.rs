//! # DiagnosticService — deciding when to publish what a file's parse found
//!
//! Two shapes of work, and the difference is who asked:
//!
//! ```text
//! a file changed    wait `interval` ms, then publish that one file's diagnostics      (the typing case)
//! the project is in publish every indexed file's diagnostics, with a progress bar     (the startup case)
//! ```
//!
//! The delay is what keeps a server from publishing on every keystroke: a client sends a change per character, and a
//! diagnostic per character is both useless (it is about text the user is still writing) and expensive. It is a
//! **debounce with cancellation** — a newer change cancels the pending pass for that file, and the newest one
//! publishes once the typing pauses.
//!
//! # What a diagnostic is, here
//!
//! A file's **parse errors**, from `handlers::diagnostic::diagnose_file`. That is deliberately the whole of it: a
//! parse is per-file work with no index behind it, so it can be answered while the project is still being read,
//! and it is the one class of diagnostic that is never a conclusion about code the analysis has not seen. Semantic
//! diagnostics (an unresolved include, a name nobody declares) need the index to be complete — the boundary
//! [`Session::pending`](cpp_code_analysis::Session::pending) draws — and are the next piece of work, not something
//! this service invents a schedule for.
//!
//! # What this does not do
//!
//! It does not decide *whether* the client wants push diagnostics. It is called by the update path, which knows
//! whether the client supports pull diagnostics (`LspFeatures::supports_pull_diagnostic`), and a client that pulls
//! is never pushed to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use log::debug;
use lsp_types::{PublishDiagnosticsParams, Uri};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{AnalysisState, ClientProxy, ProgressTask, StatusBar};
use crate::handlers::diagnose_file;

/// How long a file's diagnostics wait for the typing to stop, when the caller does not say.
pub const DEFAULT_DIAGNOSTIC_INTERVAL: u64 = 500;

pub struct DiagnosticService {
    analysis: Arc<AnalysisState>,
    client: Arc<ClientProxy>,
    status_bar: Arc<StatusBar>,
    /// One token per file, so a newer edit cancels the pass a previous edit scheduled.
    diagnostic_tokens: Arc<Mutex<HashMap<PathBuf, CancellationToken>>>,
    workspace_diagnostic_token: Arc<Mutex<Option<CancellationToken>>>,
    /// One workspace pass at a time: a second request replaces the first rather than running beside it.
    workspace_run_lock: Arc<Mutex<()>>,
}

impl DiagnosticService {
    pub fn new(
        analysis: Arc<AnalysisState>,
        status_bar: Arc<StatusBar>,
        client: Arc<ClientProxy>,
    ) -> Self {
        Self {
            analysis,
            client,
            status_bar,
            diagnostic_tokens: Arc::new(Mutex::new(HashMap::new())),
            workspace_diagnostic_token: Arc::new(Mutex::new(None)),
            workspace_run_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Publish `path`'s diagnostics once it has been quiet for `interval` milliseconds.
    ///
    /// A file with no session behind it — the client opened a file before a workspace root was named — publishes
    /// nothing and says so in the log, rather than publishing an empty list: "I have not read this" and "this file
    /// has no errors" are different statements, and only one of them is true.
    pub async fn add_diagnostic_task(&self, path: PathBuf, interval: u64) {
        let cancel_token = {
            let mut tokens = self.diagnostic_tokens.lock().await;
            if let Some(token) = tokens.get(&path) {
                token.cancel();
                debug!("cancelled the pending diagnostics for {}", path.display());
            }

            let cancel_token = CancellationToken::new();
            tokens.insert(path.clone(), cancel_token.clone());
            cancel_token
        };

        let analysis = self.analysis.clone();
        let client = self.client.clone();
        let tokens = self.diagnostic_tokens.clone();
        // The map is keyed by the path the caller gave, and the pass below consumes its own copy.
        let key = path.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(interval)) => {
                    let published = analysis
                        .run_blocking(move |session| diagnose_file(session, &path))
                        .await;

                    match published {
                        Some((uri, diagnostics)) => {
                            client.publish_diagnostics(PublishDiagnosticsParams {
                                uri,
                                diagnostics,
                                version: None,
                            });
                        }
                        None => debug!("nothing to diagnose: the file could not be read"),
                    }

                    tokens.lock().await.remove(&key);
                }
                _ = cancel_token.cancelled() => {
                    debug!("a newer change replaced the pending diagnostics");
                }
            }
        });
    }

    /// [`DiagnosticService::add_diagnostic_task`] for a list, all with the same interval.
    pub async fn add_files_diagnostic_task(&self, paths: Vec<PathBuf>, interval: u64) {
        for path in paths {
            self.add_diagnostic_task(path, interval).await;
        }
    }

    /// Tell the client this file has no diagnostics — how a client's list is cleared.
    pub fn clear_push_file_diagnostics(&self, uri: Uri) {
        self.client.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            diagnostics: vec![],
            version: None,
        });
    }

    /// Publish every indexed file's diagnostics, once the workspace has been read.
    ///
    /// The pass a client that does not pull diagnostics needs: without it, a project's parse errors appear only as
    /// the user opens each file. It runs against the session's index — the files the analysis has actually read —
    /// rather than against a directory walk, so it publishes about the project as it is understood and not about
    /// every file that happens to be on disk.
    pub async fn add_workspace_diagnostic_task(&self, interval: u64) {
        let cancel_token = {
            let mut token = self.workspace_diagnostic_token.lock().await;
            if let Some(token) = token.as_ref() {
                token.cancel();
                debug!("cancelled the pending workspace diagnostics");
            }
            token.replace(CancellationToken::new())
        };
        let Some(cancel_token) = cancel_token else {
            return;
        };

        let analysis = self.analysis.clone();
        let client = self.client.clone();
        let status_bar = self.status_bar.clone();
        let run_lock = self.workspace_run_lock.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(interval)) => {
                    // One pass at a time, and a newer pass replaces this one rather than running beside it.
                    let _guard = run_lock.lock().await;
                    if cancel_token.is_cancelled() {
                        return;
                    }

                    let paths = analysis
                        .with_snapshot(|session| {
                            session.index().summaries().map(|summary| summary.path.clone()).collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let total = paths.len();

                    status_bar.create_progress_task(ProgressTask::DiagnoseWorkspace).await;
                    let mut published = 0;

                    for path in paths {
                        if cancel_token.is_cancelled() {
                            break;
                        }

                        let result = analysis
                            .run_blocking(move |session| diagnose_file(session, &path))
                            .await;
                        if let Some((uri, diagnostics)) = result {
                            client.publish_diagnostics(PublishDiagnosticsParams {
                                uri,
                                diagnostics,
                                version: None,
                            });
                        }

                        published += 1;
                        status_bar.update_progress_task(
                            ProgressTask::DiagnoseWorkspace,
                            percentage(published, total),
                            Some(format!("{published} of {total} files")),
                        );
                    }

                    status_bar.finish_progress_task(
                        ProgressTask::DiagnoseWorkspace,
                        Some(format!("{published} files diagnosed")),
                    );
                }
                _ = cancel_token.cancelled() => {
                    debug!("the workspace diagnostic pass was replaced");
                }
            }
        });
    }

    /// Cancel every pending per-file pass — used when the workspace is replaced, because a pending diagnostic is
    /// about a session that no longer exists.
    pub async fn cancel_all(&self) {
        let mut tokens = self.diagnostic_tokens.lock().await;
        for token in tokens.values() {
            token.cancel();
        }
        tokens.clear();
        drop(tokens);

        self.cancel_workspace_diagnostic().await;
    }

    pub async fn cancel_workspace_diagnostic(&self) {
        let mut token = self.workspace_diagnostic_token.lock().await;
        if let Some(token) = token.as_ref() {
            token.cancel();
        }
        token.take();
    }
}

/// The percentage of a pass, or `None` when there is nothing to be a percentage of.
fn percentage(done: usize, total: usize) -> Option<u32> {
    (total > 0).then(|| ((done as f64 / total as f64) * 100.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::percentage;

    #[test]
    fn a_percentage_needs_something_to_measure() {
        assert_eq!(percentage(0, 0), None);
        assert_eq!(percentage(0, 4), Some(0));
        assert_eq!(percentage(2, 4), Some(50));
        assert_eq!(percentage(4, 4), Some(100));
    }
}
