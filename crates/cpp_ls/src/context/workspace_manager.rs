//! # WorkspaceManager — reloads, and the debounce in front of them
//!
//! One question, asked in two ways: **the project's build description changed, so what should be re-read?** For this
//! server that description is `compile_commands.json` — the flags, the include paths, the file list — and it changes
//! whenever a build system runs (`cmake`, a `configure`, a fresh `bear` trace). Those events arrive in bursts, so a
//! reload is debounced: the newest event cancels the pending wait, and one reload runs after the burst.
//!
//! ```text
//! the database changed ──▶ a 2 s quiet period ──▶ the workspace is re-opened
//!                          (a newer event cancels this one's wait)
//! ```
//!
//! A reload is: replace the session (the compiler is run again, the new flags are read, the summaries that have not
//! changed are served from the cache), re-mark the open buffers, index, publish the diagnostics again. It is the
//! same code path as the first `initialized` — `handlers::initialized::start_analysis` — because a reload and an
//! open differ only in what was there before.
//!
//! # Why the state and the orchestration are separate types
//!
//! [`WorkspaceState`] is data: folders, buffers, the client's configuration. This type is what *changes* it, and it
//! is deliberately the only writer — the lock order `docs/ls-architecture.md` §4 fixes is `workspace_manager`
//! before `analysis`, and keeping the mutations here is what makes that rule checkable by reading one file.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::workspace_state::WorkspaceState;
use super::{DiagnosticService, ServerContextSnapshot};
use crate::handlers::ClientConfig;

/// How long the events have to stop before a reload runs.
///
/// Two seconds because the events do not stop when a build system is done with the file: a `cmake` run writes it,
/// then the editor's watcher reports it, then the build writes it again for the next target. A reload costs a
/// compiler run and a re-read of the database, so waiting is cheap and reloading twice is not.
const CONFIG_RELOAD_DELAY: Duration = Duration::from_secs(2);

pub struct WorkspaceManager {
    config_reload_token: Arc<PendingTask>,
    reload_lock: Arc<AsyncMutex<()>>,
    reload_generation: Arc<AtomicU64>,
    file_diagnostic: Arc<DiagnosticService>,
    /// Bumped whenever the workspace changes underneath the analysis, so a background pass can see that it has been
    /// superseded and stop instead of writing into a workspace that no longer exists.
    workspace_version: Arc<AtomicI64>,
    pub state: WorkspaceState,
}

impl WorkspaceManager {
    pub fn new(file_diagnostic: Arc<DiagnosticService>) -> Self {
        Self {
            config_reload_token: Arc::new(PendingTask::default()),
            reload_lock: Arc::new(AsyncMutex::new(())),
            reload_generation: Arc::new(AtomicU64::new(0)),
            file_diagnostic,
            workspace_version: Arc::new(AtomicI64::new(0)),
            state: WorkspaceState::new(ClientConfig::default()),
        }
    }

    /// Open the workspace over the folders the client named.
    pub fn set_roots(&mut self, roots: Vec<PathBuf>) {
        if roots.len() > 1 {
            log::warn!(
                "{} workspace folders were opened; analysing the first one until multi-root is implemented",
                roots.len()
            );
        }

        self.state.set_roots(roots);
    }

    /// The root the analysis runs over.
    pub fn root(&self) -> Option<&std::path::Path> {
        self.state.root()
    }

    pub fn set_client_config(&mut self, client_config: ClientConfig) {
        self.state.set_client_config(client_config);
    }

    /// A number that changes whenever the workspace does.
    pub fn workspace_version(&self) -> i64 {
        self.workspace_version.load(Ordering::Acquire)
    }

    /// Is this path the project's build description?
    ///
    /// The question is answered by the session's own filter (`WatchFilter::is_configuration`), so that "the
    /// configuration file" means one thing in this server: the file the analysis reads to configure itself.
    pub fn is_configuration_file(&self, path: &std::path::Path) -> bool {
        self.state
            .watch_filter()
            .is_some_and(|filter| filter.is_configuration(path))
    }

    /// Schedule a reload because a configuration file changed, replacing any reload already pending.
    pub async fn add_update_config_task(&self, context: ServerContextSnapshot, config_path: PathBuf) {
        if !self.is_configuration_file(&config_path) {
            return;
        }

        let (cancel_token, cancelled_existing) =
            self.config_reload_token.replace(CONFIG_RELOAD_DELAY).await;
        if cancelled_existing {
            log::debug!("a reload was already pending; waiting for the events to stop");
        }

        let config_reload_token = self.config_reload_token.clone();
        let handles = self.reload_task_handles();
        tokio::spawn(async move {
            cancel_token.wait().await;
            if cancel_token.is_cancelled() {
                // A newer event owns the wait now: it will see the newer database, so this one does nothing.
                config_reload_token.clear(&cancel_token).await;
                return;
            }

            log::info!("reloading the workspace: {} changed", config_path.display());
            spawn_workspace_reload_task(handles, context);
            config_reload_token.clear(&cancel_token).await;
        });
    }

    /// Schedule a reload now — for a change of *folders*, which is not a debounced event.
    pub fn add_reload_workspace_task(&self, context: ServerContextSnapshot) {
        spawn_workspace_reload_task(self.reload_task_handles(), context);
    }

    /// Drop what belongs to the workspace being left behind, and mark the workspace as changed.
    ///
    /// The published diagnostics are cleared: a path in the old workspace may be a different file in the new one,
    /// and a client that kept the old errors would show them against text nobody has parsed. The buffers stay —
    /// they are the editor's, not the workspace's.
    pub async fn clear_workspace(&self) {
        self.file_diagnostic.cancel_all().await;
        self.workspace_version.fetch_add(1, Ordering::AcqRel);
    }

    fn reload_task_handles(&self) -> ReloadTaskHandles {
        ReloadTaskHandles {
            reload_lock: self.reload_lock.clone(),
            reload_generation: self.reload_generation.clone(),
            workspace_version: self.workspace_version.clone(),
        }
    }
}

impl Deref for WorkspaceManager {
    type Target = WorkspaceState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for WorkspaceManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

/// What a reload needs to own while it runs, so the spawned task holds no borrow of the manager.
#[derive(Clone)]
struct ReloadTaskHandles {
    reload_lock: Arc<AsyncMutex<()>>,
    reload_generation: Arc<AtomicU64>,
    workspace_version: Arc<AtomicI64>,
}

/// Reload the workspace, unless a newer reload has started while this one waited for the lock.
///
/// The generation counter is why this is not just a spawn: two reloads in a row must not both do their work — the
/// later one's answer is the only true one, and the earlier would publish a state that no longer exists.
fn spawn_workspace_reload_task(handles: ReloadTaskHandles, context: ServerContextSnapshot) {
    let generation = handles.reload_generation.fetch_add(1, Ordering::AcqRel) + 1;

    tokio::spawn(async move {
        let _reload_guard = handles.reload_lock.lock().await;
        if generation != handles.reload_generation.load(Ordering::Acquire) {
            return;
        }

        {
            let workspace_manager = context.workspace_manager().lock().await;
            workspace_manager.clear_workspace().await;
        }

        // The same path as the first open: the compiler runs again, the new flags are read, the buffers are
        // re-marked, the project is indexed, and the diagnostics are published again.
        crate::handlers::start_analysis(context.clone()).await;

        handles.workspace_version.fetch_add(1, Ordering::AcqRel);
    });
}

/// One pending reload. A newer event **cancels** it, and the newest event's wait is the one that ends in a reload.
#[derive(Debug)]
struct DebounceToken {
    cancelled: CancellationToken,
    quiet: Duration,
}

impl DebounceToken {
    fn new(quiet: Duration) -> Self {
        Self {
            cancelled: CancellationToken::new(),
            quiet,
        }
    }

    /// Wait out the quiet period, or return as soon as this token is superseded.
    async fn wait(&self) {
        let _ = tokio::time::timeout(self.quiet, self.cancelled.cancelled()).await;
    }

    /// This token is superseded: the new event's wait replaces it, so this one must not reload.
    fn cancel(&self) {
        self.cancelled.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }
}

#[derive(Debug, Default)]
struct PendingTask(AsyncMutex<Option<Arc<DebounceToken>>>);

impl PendingTask {
    async fn replace(&self, delay: Duration) -> (Arc<DebounceToken>, bool) {
        let mut current = self.0.lock().await;
        let had_existing = current.is_some();
        if let Some(token) = current.as_ref() {
            token.cancel();
        }

        let next = Arc::new(DebounceToken::new(delay));
        current.replace(next.clone());
        (next, had_existing)
    }

    async fn clear(&self, finished: &Arc<DebounceToken>) {
        let mut current = self.0.lock().await;
        if current
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, finished))
        {
            current.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{DebounceToken, PendingTask};

    #[tokio::test]
    async fn a_superseded_wait_ends_immediately_and_says_so() {
        // The whole point of the debounce: the newest event's wait is the one that ends in a reload, and the one it
        // replaced has to wake up and find out that it must not reload.
        let pending = PendingTask::default();
        let (first, _) = pending.replace(Duration::from_secs(60)).await;

        let started = Instant::now();
        let (second, replaced) = pending.replace(Duration::from_secs(60)).await;
        first.wait().await;

        assert!(replaced, "the caller is told that it superseded a wait");
        assert!(
            first.is_cancelled(),
            "the superseded wait knows it must not reload"
        );
        assert!(!second.is_cancelled());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn an_unsuperseded_wait_finishes_after_its_quiet_period() {
        let token = DebounceToken::new(Duration::from_millis(20));
        let started = Instant::now();

        token.wait().await;

        assert!(!token.is_cancelled());
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "the quiet period is what the reload waits for"
        );
    }

    #[tokio::test]
    async fn clearing_a_stale_token_leaves_the_newer_one_pending() {
        let pending = PendingTask::default();
        let (first, _) = pending.replace(Duration::from_millis(1)).await;
        let (second, _) = pending.replace(Duration::from_millis(1)).await;
        assert!(!second.is_cancelled());

        // The superseded task finishes *after* the one that replaced it started: clearing must not take the newer
        // wait away, or a reload would be dropped on the floor by the reload it replaced.
        pending.clear(&first).await;

        let (third, had_existing) = pending.replace(Duration::from_millis(1)).await;
        assert!(
            had_existing,
            "the newer token was still pending, so replacing it found one"
        );
        assert!(second.is_cancelled(), "and it is the one that got cancelled");
        assert!(!third.is_cancelled());
    }
}
