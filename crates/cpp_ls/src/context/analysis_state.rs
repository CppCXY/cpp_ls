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
//!
//! # What a request waits for, and the `settle` that used to make it worse
//!
//! ```text
//!   a request's wait  =  (the work the request does)
//!                     +  (the time until the lock the request needs is free)
//! ```
//!
//! The second term has two sources, and both were removed:
//!
//! * **the pump's write**, which is what the pump holds the session for while inserting. Every part of a drain that
//!   is not an insert has been moved out from under it — the parse ([`cpp_code_analysis::Session::prepare_a_wave`]),
//!   the second pass ([`cpp_code_analysis::Session::prepare_the_drain`]), the macro environments and the cook's
//!   materials ([`cpp_code_analysis::Session::materials_for`]) — so a write is 0–25 ms measured, where a slice that
//!   committed *no files at all* used to hold it for 572 ms;
//! * **the pump's read**, which it holds while parsing and which a request's *write* must wait for. This is the one
//!   that cannot be tuned away: parsing has to read the index. What removed it is that a request **no longer asks for
//!   the write lock when it has nothing to write** — see [`AnalysisState::prepare`] and [`AnalysisState::catch_up`],
//!   which ask a question about one file under the read lock and take the write one only when the answer is "there is
//!   work".
//!
//! There used to be a third, and it is worth keeping on the record because it was written twice: `settle` asked
//! whether the **whole project's** indexing queue had drained and waited up to two seconds for the answer to become
//! yes. Every answer in this server is built to **degrade rather than lie** — a name whose declaration has not been
//! read yet is skipped — so an index that is still filling gives *fewer* items and never wrong ones, and on its own
//! that makes the first minute of a session look broken: measured on a report, opening a file showed **no inlay hints
//! at all** and a completion list of nothing but keywords while hover worked.
//!
//! It was asked in eight handlers, and the question it asked was the wrong size:
//!
//! ```text
//!   what the wait was a question about     the whole project
//!   what the answer is a statement about   one file
//! ```
//!
//! Measured over the wire, twelve completions fired from `didOpen`: **988 / 536 / 1443 / 877 / 542 / 734 / 584 /
//! 1479 / 532 / 728 / 353 / 4 ms**. With the wait gone and the per-file reads in its place — the file's text, the
//! file's summary and the includes nobody has read, the modules it imports — they take **11 / 8 / 4 / 9 / 0 … 7 ms**,
//! with the list the same length or longer at every attempt. clangd states the rule this follows in as many words:
//! *"we don't wait for it to be up-to-date. Since completion is extremely time sensitive, it just uses whichever is
//! immediately available."*
//!
//! An empty popup is still the symptom to watch for. The fix for it is the hot set and the per-file reads, **never a
//! global wait**.

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
    /// **The longest an update has held `inner`**, in milliseconds — see [`AnalysisState::update`].
    worst_write_hold: Arc<std::sync::atomic::AtomicU64>,
    /// "The queue has work in it" — how an edit reaches the task that reads.
    work: Arc<Notify>,
}

/// **How long an update may hold the analysis before it is a defect.**
///
/// A query and an update take the same lock, so this is a bound on how long any request can be made to wait by the
/// background: not on average, not usually — **by construction**, because there is no other lock on the path and a
/// request that is slow for another reason is slow inside its own work.
///
/// The number is chosen from what the work is: an update **inserts what a step produced**, and inserting is a map
/// write per declaration. Measured, the insert half of an index slice is 8–19 ms. Fifty leaves room for a slice
/// that has to grow a table and still fails a closure that parses a file, walks a unit, or runs a second pass.
pub const WRITE_BUDGET: Duration = Duration::from_millis(50);

/// **How long a read-only query may take before it is worth a line in the log.**
///
/// The read side's counterpart of [`WRITE_BUDGET`], and deliberately the same number: a keystroke's budget is around
/// 16 ms and this server's answer to one is a query, so a query over fifty milliseconds is either waiting for
/// something or doing too much — and [`AnalysisState::run_blocking`] says which, in four numbers that fail
/// separately.
///
/// Reported rather than enforced: a query's own work is allowed to be as large as the question is (a completion
/// after `std::` collects thousands of declarations), and the point of the line is to make that visible rather than
/// to bound it.
pub const QUERY_BUDGET: Duration = Duration::from_millis(50);

impl AnalysisState {
    pub fn new() -> Self {
        let documents = OpenDocuments::new();
        let files = SessionFiles::new(documents, DiskFiles);
        Self {
            inner: Arc::new(RwLock::new(None)),
            files,
            gate: Arc::new(tokio::sync::RwLock::new(())),
            blocking_permits: Arc::new(Semaphore::new(Self::analysis_parallelism())),
            worst_write_hold: Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
        // **The shortcut that matters, and it is the ordinary case.** The file a request is about is normally one the
        // editor has open, so the VFS already holds its text and its cooked reading is either built or already asked
        // for — which is exactly the state in which the two steps below would each do nothing. Asking is a read, and
        // a read takes the lock the pump takes **as a reader**, so it does not wait for the pump's parse; the write
        // would.
        //
        // Measured over the wire on the latency fixture before this: a completion spent **9–1147 ms** here and every
        // millisecond of it was the queue — the work is a `HashSet` lookup and a map lookup.
        //
        // `false` from the snapshot (no workspace open) falls through to the write, which answers `Missing` the same
        // way it always did.
        if self
            .with_snapshot(|session| session.is_ready_for_a_request_about(path))
            .unwrap_or(false)
        {
            return true;
        }

        let path = path.to_path_buf();
        self.update_session("prepare a file a request named", move |session| {
            let loaded = session.load(&path).is_some();
            // **A file a request is about gets its cooked reading** — and this is the only place that knows which
            // file a request named. Not "cook it now": the query below answers from the reading that exists (its own
            // text, when there is nothing else), and the *next* request gets the compiler's reading, which is what
            // `isIncomplete` tells a client to come back for. The session's rule is that a reading is built when
            // something looks at the file; this is the "something" for everything the editor did not open.
            if loaded {
                session.want_cooked_reading(&path);
            }
            loaded
        })
        .await
        .unwrap_or(false)
    }

    /// **Read the file a request is about in, if the analysis is behind it** — the request path's form of
    /// [`Session::catch_up`].
    ///
    /// # Why this is not just `update_session(|s| s.catch_up(path))`
    ///
    /// Because the lock is the whole cost. The pump parses a project's include closure while **holding the session
    /// for reading** — measured, a hundred milliseconds a section and sometimes more than a second — and a request
    /// that asks for the write lock waits for the section in flight. `catch_up`'s own work is conditional: the file
    /// when an edit dropped its summary, and the includes the index has never read. For the ordinary request both
    /// are empty and the call is a `HashSet` lookup, paid for with a second of queueing.
    ///
    /// Measured over the wire on the latency fixture, with the same shortcut already in place for
    /// [`AnalysisState::prepare`]: `catch_up` cost **0–1108 ms** on a file whose summary was current.
    ///
    /// The check is a **read**, and a read does not wait for the pump's parse — only a write does. See
    /// [`Session::needs_catching_up`] for why the answer is exact rather than a guess.
    pub async fn catch_up(&self, path: &std::path::Path) {
        if !self
            .with_snapshot(|session| session.needs_catching_up(path))
            .unwrap_or(true)
        {
            return;
        }

        let path = path.to_path_buf();
        self.update_session("a request caught up with its file", move |session| {
            session.catch_up(&path)
        })
        .await;
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
        self.update("open the session", move |slot| {
            *slot = Some(Session::open_with_config_file(
                root,
                files,
                filter,
                config_file.as_deref(),
            ));
        })
        .await;
    }

    /// **Run a read-only query, and report where a slow one spent its time.**
    ///
    /// # The measurement, and why the read path needed one of its own
    ///
    /// [`AnalysisState::update`] has reported the longest it held the analysis since the first round of this work,
    /// and that number bounds what a *write* can be made to wait. It says nothing about a **read**, and the two fail
    /// separately — which is the distinction `crates/cpp_ls/tests/latency.rs` was written around and the one that got
    /// lost for three rounds. A query can be slow for three unrelated reasons and this line tells them apart:
    ///
    /// ```text
    ///   wait permit   every core is already running a query — the analysis pool is saturated
    ///   wait gate     an update is in flight: `AnalysisState::update` takes the gate for writing first
    ///   wait session  a writer holds the session, or one is waiting for it (a `SRWLOCK` queues new readers behind
    ///                 a waiting writer, so this is where the pump's own commit shows up)
    ///   ran           the query's own work — reading the index, parsing the file, resolving the scope
    /// ```
    ///
    /// Measured on the latency fixture, this is what separated "the completion waits for the pump" from "the
    /// completion's own query is slow": with the writes in good order the first five completions after `didOpen`
    /// still took 60–1677 ms, and every millisecond of it was in this function's `ran` — the query's own work over an
    /// index that was still filling.
    pub async fn run_blocking<R, F>(&self, f: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&Session<DiskFiles>) -> Option<R> + Send + 'static,
    {
        let started = std::time::Instant::now();
        let _permit = self.blocking_permits.clone().acquire_owned().await.ok()?;
        let waited_for_a_permit = started.elapsed().as_millis() as u64;

        let inner = self.inner.clone();
        let gate = self.gate.clone().read_owned().await;
        let waited_for_the_gate = started.elapsed().as_millis() as u64;

        let result = tokio::task::spawn_blocking(move || {
            let locked = std::time::Instant::now();
            let session = inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let waited_for_the_session = locked.elapsed().as_millis() as u64;
            // The gate is released before the query runs: it orders an update against the queries already in
            // flight, and holding it for the whole query would make every notification queue behind every read.
            drop(gate);
            let ran = std::time::Instant::now();
            // `?` cannot be used here — the closure answers with the timings as well — so `None` is threaded by hand:
            // "no session has been opened" is the one thing the caller distinguishes, and it is the same answer
            // whether the session was missing or the query declined.
            let answer = session.as_ref().and_then(f);
            (
                waited_for_the_session,
                ran.elapsed().as_millis() as u64,
                answer,
            )
        })
        .await;

        match result {
            Ok((waited_for_the_session, ran, answer)) => {
                let took = started.elapsed().as_millis() as u64;
                if took > QUERY_BUDGET.as_millis() as u64 {
                    log::debug!(
                        "a query took {took} ms — permit {waited_for_a_permit} ms, gate {gate} ms, session \
                         {session} ms, ran {ran} ms",
                        gate = waited_for_the_gate - waited_for_a_permit,
                        session = waited_for_the_session,
                    );
                }
                answer
            }
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
    /// `label` names the caller for [`AnalysisState::update`]'s budget warning — see the note there.
    pub async fn update_session<R>(
        &self,
        label: &'static str,
        f: impl FnOnce(&mut Session<DiskFiles>) -> R,
    ) -> Option<R> {
        self.update(label, |slot| slot.as_mut().map(f)).await
    }

    /// Change the analysis — including opening and closing the session itself.
    ///
    /// The closure sees the slot rather than a session because this is the one path that can *create* one
    /// ([`AnalysisState::open`] is written in terms of it); an update that has a session to work on wants
    /// [`AnalysisState::update_session`].
    ///
    /// # Which caller was slow, and why it is a label rather than `#[track_caller]`
    ///
    /// `#[track_caller]` is **a no-op on an `async fn`** — rustc says so in a warning, and the first version of this
    /// did exactly that and reported `analysis_state.rs:308` for every slow update, which is this function rather
    /// than the caller. A label is something a caller has to remember to pass, and that is its whole cost; what it
    /// buys is a name in the warning that says which of a dozen call sites did the work.
    pub async fn update<R>(
        &self,
        label: &'static str,
        f: impl FnOnce(&mut Option<Session<DiskFiles>>) -> R,
    ) -> R {
        let inner = self.inner.clone();
        let gate = self.gate.clone().write_owned().await;
        let held = self.worst_write_hold.clone();
        let run = move || {
            let mut slot = inner
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(gate);
            // **The number this server is judged by, taken by the server.**
            //
            // A query waits for whatever this closure does, so the *only* thing that makes this product slow is a
            // closure that takes long — and there is no way to see that from the outside: the request it delayed
            // reports its own total, which also includes the work the request asked for. So every update is timed
            // here, the worst one is kept, and anything over the budget says so with its own number in the log.
            //
            // This is the architecture rather than a diagnostic: [`WRITE_BUDGET`] is the contract, this is what
            // enforces it, and `worst_write_hold` is what a test asserts on. A fix that does not move this number
            // has not fixed anything.
            let started = std::time::Instant::now();
            let answer = f(&mut slot);
            let took = started.elapsed().as_millis() as u64;
            held.fetch_max(took, std::sync::atomic::Ordering::Relaxed);
            if took > WRITE_BUDGET.as_millis() as u64 {
                log::warn!(
                    "`{label}` held the analysis for {took} ms, over the {budget} ms budget — every request in \
                     flight waited for it",
                    budget = WRITE_BUDGET.as_millis()
                );
            }
            answer
        };
        blocking(run)
    }

    /// **The longest any update has held the analysis**, in milliseconds, since this state was created.
    ///
    /// The one number that bounds a request's wait: a query takes the same lock an update does, so no request can be
    /// delayed by more than this. Read by the tests, and worth reading from a log line when a report says a
    /// completion took a second — if this is small and the request was slow, the wait was inside the request's own
    /// work rather than in the queue.
    pub fn worst_write_hold(&self) -> u64 {
        self.worst_write_hold.load(std::sync::atomic::Ordering::Relaxed)
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
            .update(
                "a test's session",
                move |slot: &mut Option<Session<DiskFiles>>| {
                    *slot = Some(Session::with_config(
                        &root,
                        files,
                        WatchFilter::new(&root),
                        CompilerConfig::default(),
                    ));
                    slot.is_some()
                },
            )
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

        let updated = state.update_session("a test's update", |_| 1).await;
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
        let write = tokio::spawn(async move { write_state.update("a test's update", |_| 2).await });

        assert_eq!(read.await.unwrap(), Some(1));
        assert_eq!(write.await.unwrap(), 2);
    }
}

