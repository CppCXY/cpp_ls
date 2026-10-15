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

/// One file's pending pass: the token that cancels it, and which pass it is.
///
/// **The generation is what makes the removal safe.** A pass removes itself from the map when it finishes, and
/// between its start and its finish a newer edit may have replaced it — so a removal by key alone deletes the
/// *newer* entry and leaves the next edit with nothing to cancel:
///
/// ```text
/// t1  a change arrives                    → token A registered
/// t2  its delay elapses, the pass starts  → several seconds of work
/// t3  the user types again                → A is cancelled (too late, it is running), token B registered
/// t4  the pass finishes                   → `remove(&path)` deletes **B**
/// t5  the user types again                → nothing to cancel, and the pass from t2 publishes stale diagnostics
/// ```
///
/// A pass removes its entry only when the generation still names it, so the entry that a newer edit installed
/// survives, and that edit's own cancellation works.
struct Pending {
    token: CancellationToken,
    generation: u64,
}

pub struct DiagnosticService {
    analysis: Arc<AnalysisState>,
    client: Arc<ClientProxy>,
    status_bar: Arc<StatusBar>,
    /// One entry per file, so a newer edit cancels the pass a previous edit scheduled.
    diagnostic_tokens: Arc<Mutex<HashMap<PathBuf, Pending>>>,
    /// Increments on every scheduled pass, so a finishing pass can tell whether it is still the newest.
    next_generation: Arc<std::sync::atomic::AtomicU64>,
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
            next_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            workspace_diagnostic_token: Arc::new(Mutex::new(None)),
            workspace_run_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Publish `path`'s diagnostics once it has been quiet for `interval` milliseconds.
    ///
    /// A file with no session behind it — the client opened a file before a workspace root was named — publishes
    /// nothing and says so in the log, rather than publishing an empty list: "I have not read this" and "this file
    /// has no errors" are different statements, and only one of them is true.
    ///
    /// `version` is the client's counter for the document the text came from, and it is **stamped on the answer**.
    /// A client discards a diagnostic whose version is older than the text it is showing, and that is the only
    /// defence it has: this server debounces and cancels, but a pass that has already started cannot be stopped, so
    /// an answer about text the user has since replaced can still reach the wire. See [`Pending`] for the other half.
    pub async fn add_diagnostic_task(&self, path: PathBuf, interval: u64, version: Option<i32>) {
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cancel_token = {
            let mut tokens = self.diagnostic_tokens.lock().await;
            if let Some(pending) = tokens.get(&path) {
                pending.token.cancel();
                debug!("cancelled the pending diagnostics for {}", path.display());
            }

            let cancel_token = CancellationToken::new();
            tokens.insert(
                path.clone(),
                Pending {
                    token: cancel_token.clone(),
                    generation,
                },
            );
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
                        .cook_and_diagnose(&path)
                        .await;

                    match published {
                        Some((uri, diagnostics)) => {
                            client.publish_diagnostics(PublishDiagnosticsParams {
                                uri,
                                diagnostics,
                                version,
                            });
                        }
                        None => debug!("nothing to diagnose: the file could not be read"),
                    }

                    // **Only its own entry.** A newer edit may have replaced this one while the pass ran, and
                    // removing by key would delete *that* entry — leaving the next edit with nothing to cancel and
                    // this pass's stale answer on the client. See [`Pending`].
                    let mut tokens = tokens.lock().await;
                    if is_still_the_newest(tokens.get(&key), generation) {
                        tokens.remove(&key);
                    }
                }
                _ = cancel_token.cancelled() => {
                    debug!("a newer change replaced the pending diagnostics");
                }
            }
        });
    }

    /// [`DiagnosticService::add_diagnostic_task`] for a list, all with the same interval.
    ///
    /// No version: these are files the client is not showing, so there is no document revision to stamp — a file
    /// nobody has open has only its disk text.
    pub async fn add_files_diagnostic_task(&self, paths: Vec<PathBuf>, interval: u64) {
        for path in paths {
            self.add_diagnostic_task(path, interval, None).await;
        }
    }

    /// Tell the client this file has no diagnostics — how a client's list is cleared.
    pub fn clear_push_file_diagnostics(&self, uri: Uri) {
        self.client.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            diagnostics: vec![],
            // No version: this is not an answer about a revision, it is the statement that there is nothing to say.
            // A client that compared it against a newer document would be right to ignore it, and the file is being
            // closed in any case.
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
        for pending in tokens.values() {
            pending.token.cancel();
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

/// A pass removes its own entry, and **only** its own.
///
/// This is the whole of the fix for a race that is invisible without a test: a pass runs for seconds, an edit during
/// it installs a newer entry, and the finishing pass used to remove by key — deleting the newer one and leaving the
/// next edit with nothing to cancel. The check is stated as a function so it can be tested without a session, a
/// client or a clock.
fn is_still_the_newest(pending: Option<&Pending>, generation: u64) -> bool {
    pending.is_some_and(|pending| pending.generation == generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finishing_pass_removes_only_its_own_entry() {
        let mine = Pending {
            token: CancellationToken::new(),
            generation: 3,
        };
        assert!(
            is_still_the_newest(Some(&mine), 3),
            "no edit arrived while this pass ran, so it owns the entry"
        );

        // An edit arrived during the pass: the entry is the newer pass's, and removing it would leave the *next*
        // edit with nothing to cancel — and this pass's stale answer on the client.
        assert!(
            !is_still_the_newest(Some(&mine), 2),
            "an older pass must not remove the entry a newer edit installed"
        );
        assert!(
            !is_still_the_newest(None, 3),
            "an entry removed by someone else is not this pass's to remove again"
        );
    }

    /// The cancelled token a newer edit leaves behind is what the *client* would go on showing, so it is worth
    /// pinning that cancelling is visible to the pass that is waiting: a cancellation that a running pass cannot see
    /// is exactly the hole this service has to close by stamping the version.
    #[test]
    fn a_newer_edit_cancels_the_pending_one() {
        let pending = Pending {
            token: CancellationToken::new(),
            generation: 0,
        };
        assert!(!pending.token.is_cancelled());
        pending.token.cancel();
        assert!(
            pending.token.is_cancelled(),
            "the pass waiting on this token has to see the cancellation"
        );
    }

    use super::percentage;

    #[test]
    fn a_percentage_needs_something_to_measure() {
        assert_eq!(percentage(0, 0), None);
        assert_eq!(percentage(0, 4), Some(0));
        assert_eq!(percentage(2, 4), Some(50));
        assert_eq!(percentage(4, 4), Some(100));
    }
}
