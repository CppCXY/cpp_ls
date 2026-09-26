//! # AnalysisState — the analysis session and the buffers, behind a read-write lock
//!
//! One [`Session`] for the workspace: created when the client says where the workspace is, read by every request
//! under a read lock, written by the notification worker under a write lock. A query does **not** clone the analysis
//! to run — a read guard *is* the session, and the query reads through it while other readers do the same.
//!
//! # Why the session is an `Option`, and what a query answers without one
//!
//! The server's two halves learn different things at different times: `initialize` builds this state, and
//! `initialized` (or the first `workspace/didChangeWorkspaceFolders`) is what names a root directory — and the root
//! is what a session is opened over, because opening one runs the compiler and reads `compile_commands.json`
//! (`Session::open`). So there is a window in which a request can arrive with no analysis behind it, and the answer
//! there is [`RequestOutcome::Missing`] with a log line: "the analysis has not been opened" is a true statement, and
//! an empty result set would be a false one. The same rule the analysis layer applies to `Unknown` applies here.
//!
//! # Why the providers are held here as well as by the session
//!
//! Because there are two users of one chain: the session owns a handle to it (that is how it reads a file), and the
//! client's notifications arrive with text that has to reach the same chain. `SessionFiles` is a **handle** — the
//! buffers are a map behind a lock that clones share — so `files` and `documents` here and the session's copy are
//! the same files, not copies of them. See `cpp_code_analysis::session`'s module documentation.

use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use tokio::sync::{Notify, Semaphore};

use crate::context::RequestOutcome;

pub struct AnalysisState {
    /// The session, from the moment the workspace root is known. `None` before that; see the module documentation.
    inner: Arc<RwLock<Option<Session<DiskFiles>>>>,
    /// The provider chain a session is opened over, and the handle this state keeps after it is.
    files: SessionFiles<DiskFiles>,
    /// How many queries may read at once.
    blocking_permits: Arc<Semaphore>,
    /// Held for reading by a query and for writing by an update, so that an update waits for the queries already
    /// running rather than interleaving with them. That is the whole reason it is separate from `inner`'s own lock:
    /// the write lock is taken *before* the blocking hop, which is what keeps a notification from being overtaken.
    gate: Arc<tokio::sync::RwLock<()>>,
    /// "The queue has work in it" — how an edit reaches the task that reads.
    work: Arc<Notify>,
}

impl AnalysisState {
    pub fn new() -> Self {
        let documents = OpenDocuments::new();
        let files = SessionFiles::new(documents, DiskFiles);
        Self {
            inner: Arc::new(RwLock::new(None)),
            files,
            gate: Arc::new(tokio::sync::RwLock::new(())),
            blocking_permits: Arc::new(Semaphore::new(Self::analysis_parallelism())),
            work: Arc::new(Notify::new()),
        }
    }

    /// Read a file in and hold it, so that the read-only queries can see it.
    ///
    /// # Why a request needs this, and why it is not part of the query
    ///
    /// The VFS holds a file's text and its line index, and it has **no lock of its own**: changing what is held
    /// takes `&mut Session`, so a *query* — which runs under this state's read lock, in parallel with other queries
    /// — can only look at what is held. Filling the table is the writer's job, and two writers already do it: the
    /// notification path (`didOpen`/`didChange` hold the client's buffers, `didClose` closes them) and the indexer
    /// (every file it reads, it holds).
    ///
    /// This method is the remaining gap: a request can be about a file nothing has read yet — a client asking for
    /// diagnostics of a file it never opened. So the handler asks for the file **before** the query; the write lock
    /// is held for as long as one file takes to read, and the query itself stays a read, in parallel with every
    /// other request. (A query that loaded what it needed would take `&mut Session` and serialize all of them, which
    /// is the trade this exists to avoid.)
    pub async fn prepare(&self, path: &std::path::Path) -> bool {
        let path = path.to_path_buf();
        self.update_session(move |session| session.load(&path).is_some())
            .await
            .unwrap_or(false)
    }

    /// Tell the indexing pump that the queue has work in it.
    ///
    /// Called after a notification has changed what the analysis should read. The engine has no threads and no
    /// scheduler on purpose (`Session::advance` is "one file per call, and the caller decides how much"), so
    /// *something* in this layer has to be the caller that keeps calling — this is how that task is told there is
    /// a reason to. `Notify` rather than a channel because the signal carries no data: the queue is the message.
    pub fn wake(&self) {
        self.work.notify_one();
    }

    /// Wait for a wake-up, or for `timeout` to pass.
    ///
    /// Returns whether a wake-up arrived. The timeout is **insurance, not the mechanism**: `Notify` stores one
    /// permit, so a wake that arrives before this call is not lost — but a stall would leave the index empty for
    /// the rest of the session, and one lock acquisition a second is a cheap way never to find out.
    pub async fn wait_for_work(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.work.notified()).await.is_ok()
    }

    /// Run `f` against the live session, or answer `None` when no workspace has been opened.
    pub fn with_snapshot<T>(&self, f: impl FnOnce(&Session<DiskFiles>) -> T) -> Option<T> {
        let session = self.read();
        Some(f(session.as_ref()?))
    }

    /// Logical parallelism used by the analysis blocking pool.
    pub fn analysis_parallelism() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    }

    /// The provider chain a session is opened over — the same files the live session reads.
    ///
    /// A test's way in: it builds a session with [`Session::with_config`] over *this* chain (no compiler is run) to
    /// assert that a buffer written through one handle is what a session reads through the other. Production code
    /// does not need it, because [`AnalysisState::open`] builds the session over the same chain itself.
    #[cfg(test)]
    pub fn files(&self) -> &SessionFiles<DiskFiles> {
        &self.files
    }

    /// Make the analysis be about `root`, replacing whatever session was open.
    ///
    /// Replacing rather than refusing, because this is the same call for the two moments a workspace appears: the
    /// first `initialized`, and a reload (the workspace folder changed, `.cppls.toml` or `compile_commands.json` was
    /// rewritten). In both cases the honest answer is "analyse *this* root now" — and a reload is cheap, because
    /// the summaries the previous session wrote are on disk under the root and the new session re-reads them
    /// instead of parsing (`Session::open`, `SummaryStore`).
    ///
    /// `config_file` is a configuration file a caller named (`--config`); `None` means the conventional
    /// `.cppls.toml` in the root, and a project without one is the ordinary case rather than a problem.
    ///
    /// The buffers survive either way: they live in the provider chain, which this state owns and every session
    /// reads through. What does *not* survive is the index, so the caller re-marks the open documents
    /// (`Session::did_open`) — see `handlers::initialized`.
    pub async fn open(&self, root: PathBuf, filter: WatchFilter, config_file: Option<PathBuf>) {
        let files = self.files.clone();
        self.update(move |slot| {
            *slot = Some(Session::open_with_config_file(
                root,
                files,
                filter,
                config_file.as_deref(),
            ));
        })
        .await;
    }

    pub async fn run_blocking<R, F>(&self, f: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&Session<DiskFiles>) -> Option<R> + Send + 'static,
    {
        let _permit = self.blocking_permits.clone().acquire_owned().await.ok()?;
        let inner = self.inner.clone();
        let gate = self.gate.clone().read_owned().await;
        let result = tokio::task::spawn_blocking(move || {
            let session = inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // The gate is released before the query runs: it orders an update against the queries already in
            // flight, and holding it for the whole query would make every notification queue behind every read.
            drop(gate);
            f(session.as_ref()?)
        })
        .await;
        match result {
            Ok(value) => value,
            Err(err) => {
                if err.is_panic() {
                    std::panic::resume_unwind(err.into_panic());
                }
                None
            }
        }
    }

    pub async fn query_blocking<R, F>(&self, f: F) -> RequestOutcome<R>
    where
        R: Send + 'static,
        F: FnOnce(&Session<DiskFiles>) -> Option<R> + Send + 'static,
    {
        match self.run_blocking(f).await {
            Some(value) => RequestOutcome::Ready(value),
            None => RequestOutcome::Missing,
        }
    }

    /// Tell the analysis what a path's text is now, or answer `None` when no workspace has been opened.
    ///
    /// The notification worker's entry point: `didOpen`, `didChange`, `didSave`, `didClose` and the client's file
    /// events all reach the session through here, and all of them need it to exist.
    pub async fn update_session<R>(
        &self,
        f: impl FnOnce(&mut Session<DiskFiles>) -> R,
    ) -> Option<R> {
        self.update(|slot| slot.as_mut().map(f)).await
    }

    /// Change the analysis — including opening and closing the session itself.
    ///
    /// The closure sees the slot rather than a session because this is the one path that can *create* one
    /// ([`AnalysisState::open`] is written in terms of it); an update that has a session to work on wants
    /// [`AnalysisState::update_session`].
    pub async fn update<R>(&self, f: impl FnOnce(&mut Option<Session<DiskFiles>>) -> R) -> R {
        let inner = self.inner.clone();
        let gate = self.gate.clone().write_owned().await;
        let run = move || {
            let mut slot = inner
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(gate);
            f(&mut slot)
        };
        blocking(run)
    }

    fn read(&self) -> RwLockReadGuard<'_, Option<Session<DiskFiles>>> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Run `f` where blocking is allowed: on the server's multi-threaded runtime the thread hands its other tasks on
/// (`block_in_place`), and anywhere else — a test's current-thread runtime — it is called directly, because
/// `block_in_place` panics there.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    if tokio::runtime::Handle::try_current()
        .map(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false)
    {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

impl Default for AnalysisState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::CompilerConfig;

    /// A workspace with a session the test built itself.
    ///
    /// [`AnalysisState::open`] runs the compiler (`Session::open` asks it for its search paths), which is not
    /// something a unit test should do: the contract under test here is *this* state's — that a session exists
    /// before a query, and that the buffers are shared — and `with_config` gives it one without a toolchain.
    async fn opened(state: &AnalysisState, root: &std::path::Path) {
        let files = state.files().clone();
        let root = root.to_path_buf();
        let opened = state
            .update(move |slot| {
                *slot = Some(Session::with_config(
                    &root,
                    files,
                    WatchFilter::new(&root),
                    CompilerConfig::default(),
                ));
                slot.is_some()
            })
            .await;
        assert!(opened);
    }

    /// A buffer the client opened is the text the analysis reads, through **both** handles: the state's provider
    /// chain and the session's. This is the property the whole arrangement exists for — a second owner of a chain
    /// that is the same chain, not a copy of it.
    #[tokio::test]
    async fn a_buffer_written_through_the_handle_is_read_by_the_session() {
        let state = AnalysisState::new();
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;

        let buffer = "int from_the_handle;\n";
        state.files().overlay.open("/p/main.cpp", buffer);
        // The buffer is in the provider chain, but a *session* holds a file only once something has read it — which
        // is what the notification path does for a file the client has open (`AnalysisState::prepare`).
        assert!(state.prepare(std::path::Path::new("/p/main.cpp")).await);

        let text = state
            .with_snapshot(|session| session.view("/p/main.cpp").map(|view| view.source.clone()))
            .flatten();
        assert_eq!(text.as_deref(), Some(buffer));
    }

    /// Opening a workspace replaces the analysis, and the buffers are still there afterwards — which is what makes
    /// a reload (`compile_commands.json` rewritten, a folder re-opened) cheap rather than a restart.
    #[tokio::test]
    async fn opening_again_replaces_the_session_and_keeps_the_buffers() {
        let state = AnalysisState::new();
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;
        state.files().overlay.open("/p/main.cpp", "int first;\n");

        opened(&state, &root).await;
        // The **session** was replaced, so its file table is a new one: the buffer survives in the provider chain
        // (which outlives every session) and is read into the new table by the same step that reads any file.
        assert!(state.prepare(std::path::Path::new("/p/main.cpp")).await);

        let text = state
            .with_snapshot(|session| session.view("/p/main.cpp").map(|view| view.source.clone()))
            .flatten();
        assert_eq!(
            text.as_deref(),
            Some("int first;\n"),
            "the buffer is in the provider chain, which outlives the session"
        );
    }

    /// Before a workspace is named every query answers "no analysis", which is `Missing` — not an empty answer.
    #[tokio::test]
    async fn a_query_before_the_workspace_is_opened_is_missing() {
        let state = AnalysisState::new();

        let outcome = state
            .query_blocking(|session| Some(session.root().to_path_buf()))
            .await;
        assert!(matches!(outcome, RequestOutcome::Missing));

        let updated = state.update_session(|_| 1).await;
        assert_eq!(updated, None, "and there is nothing to update either");
    }

    /// A query and an update reach the analysis at the same time without deadlocking: the read takes a permit and
    /// the read gate, the update takes the write gate and waits for it, and both finish.
    #[tokio::test]
    async fn blocking_query_and_update_complete() {
        let state = Arc::new(AnalysisState::new());
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;

        let read_state = state.clone();
        let read = tokio::spawn(async move { read_state.run_blocking(|_| Some(1)).await });
        let write_state = state.clone();
        let write = tokio::spawn(async move { write_state.update(|_| 2).await });

        assert_eq!(read.await.unwrap(), Some(1));
        assert_eq!(write.await.unwrap(), 2);
    }
}

