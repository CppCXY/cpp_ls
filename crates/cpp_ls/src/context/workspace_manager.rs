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
//! is deliberately the only writer — the lock order fixes is `workspace_manager`
//! before `analysis`, and keeping the mutations here is what makes that rule checkable by reading one file.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use super::ServerContextSnapshot;
use super::workspace_state::WorkspaceState;
use crate::handlers::ClientConfig;

/// How long the events have to stop before a reload runs.
///
/// Two seconds because the events do not stop when a build system is done with the file: a `cmake` run writes it,
/// then the editor's watcher reports it, then the build writes it again for the next target. A reload costs a
/// compiler run and a re-read of the database, so waiting is cheap and reloading twice is not.
const RELOAD_DEBOUNCE: Duration = Duration::from_secs(2);

pub struct WorkspaceManager {
    reload_token: Arc<PendingTask>,
    reload_lock: Arc<AsyncMutex<()>>,
    reload_generation: Arc<AtomicU64>,
    /// Bumped whenever the workspace changes underneath the analysis, so a background pass can see that it has been
    /// superseded and stop instead of writing into a workspace that no longer exists.
    workspace_version: Arc<AtomicI64>,
    log_file: Option<PathBuf>,
    closing: bool,
    pub state: WorkspaceState,
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self {
            reload_token: Arc::new(PendingTask::default()),
            reload_lock: Arc::new(AsyncMutex::new(())),
            reload_generation: Arc::new(AtomicU64::new(0)),
            workspace_version: Arc::new(AtomicI64::new(0)),
            log_file: None,
            closing: false,
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

    pub fn set_log_file(&mut self, path: Option<PathBuf>) {
        self.log_file = path;
    }

    pub fn log_file(&self) -> Option<&std::path::Path> {
        self.log_file.as_deref()
    }

    pub fn set_client_config(&mut self, client_config: ClientConfig) {
        self.state.set_client_config(client_config);
    }

    /// A number that changes whenever the workspace does.
    pub fn workspace_version(&self) -> i64 {
        self.workspace_version.load(Ordering::Acquire)
    }

    pub fn reload_generation(&self) -> u64 {
        self.reload_generation.load(Ordering::Acquire)
    }

    pub fn is_closing(&self) -> bool {
        self.closing
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
    pub async fn add_update_config_task(
        &self,
        context: ServerContextSnapshot,
        config_path: PathBuf,
    ) {
        if !self.is_configuration_file(&config_path) {
            return;
        }
        self.add_debounced_reload(context, format!("{} changed", config_path.display()))
            .await;
    }

    pub async fn add_rescan_task(&self, context: ServerContextSnapshot) {
        self.add_debounced_reload(context, "native watcher requested a rescan".into())
            .await;
    }

    async fn add_debounced_reload(&self, context: ServerContextSnapshot, reason: String) {
        let (cancel_token, cancelled_existing) = self.reload_token.replace(RELOAD_DEBOUNCE).await;
        if cancelled_existing {
            log::debug!("a reload was already pending; waiting for the events to stop");
        }

        let reload_token = self.reload_token.clone();
        tokio::spawn(async move {
            cancel_token.wait().await;
            if cancel_token.is_cancelled() {
                // A newer event owns the wait now: it will see the newer database, so this one does nothing.
                reload_token.clear(&cancel_token).await;
                return;
            }

            let manager = context.workspace_manager().lock().await;
            if !manager.is_closing() && !cancel_token.is_cancelled() {
                log::info!("reloading the workspace: {reason}");
                manager.add_reload_workspace_task(context.clone());
            }
            drop(manager);
            reload_token.clear(&cancel_token).await;
        });
    }

    /// Schedule a reload now — for a change of *folders*, which is not a debounced event.
    pub fn add_reload_workspace_task(&self, context: ServerContextSnapshot) {
        spawn_workspace_reload_task(self.reload_task_handles(), context);
    }

    /// Mark the old workspace epoch as obsolete. Open buffers remain owned by the editor.
    pub async fn clear_workspace(&self) {
        self.workspace_version.fetch_add(1, Ordering::AcqRel);
    }

    pub async fn shutdown(&mut self) {
        self.closing = true;
        self.reload_generation.fetch_add(1, Ordering::AcqRel);
        self.reload_token.cancel().await;
        self.clear_workspace().await;
    }

    fn reload_task_handles(&self) -> ReloadTaskHandles {
        ReloadTaskHandles {
            reload_lock: self.reload_lock.clone(),
            reload_generation: self.reload_generation.clone(),
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
            if workspace_manager.is_closing()
                || generation != handles.reload_generation.load(Ordering::Acquire)
            {
                return;
            }
            workspace_manager.clear_workspace().await;
        }

        // The same path as the first open: the compiler runs again, the new flags are read, the buffers are
        // re-marked, the project is indexed, and the diagnostics are published again.
        crate::handlers::start_analysis(context.clone()).await;
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
    async fn cancel(&self) {
        if let Some(token) = self.0.lock().await.take() {
            token.cancel();
        }
    }

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

    use super::{DebounceToken, PendingTask, WorkspaceManager};

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
    async fn shutdown_cancels_pending_reload_and_invalidates_the_epoch() {
        let mut manager = WorkspaceManager::new();
        let (token, _) = manager.reload_token.replace(Duration::from_secs(60)).await;
        let version = manager.workspace_version();
        let generation = manager.reload_generation();
        manager.shutdown().await;
        assert!(manager.is_closing());
        assert!(token.is_cancelled());
        assert!(manager.workspace_version() > version);
        assert!(manager.reload_generation() > generation);
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
        assert!(
            second.is_cancelled(),
            "and it is the one that got cancelled"
        );
        assert!(!third.is_cancelled());
    }
}
