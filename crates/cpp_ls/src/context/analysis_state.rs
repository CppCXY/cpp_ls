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

/// **How many files one pull request may index** — the bound that was missing, and the 102 seconds it cost.
///
/// [`Session::index_everything`] loops until the queue is **empty**, so a caller that asks for it asks for the whole
/// project. A pull request did exactly that, and the budget warning caught what it means:
///
/// ```text
/// 20:22:22  index slice: prepared 128, committed 32, write lock held 563 ms, 1258 left
/// 20:24:05  `prepare a file a pull request named` held the analysis for 102700 ms
/// 20:24:09  index slice: prepared 1, committed 1, write lock held 0 ms, 1 to cook, 0 left
/// ```
///
/// The pump was 1 258 files from finished and the pull drained all of them in one closure — **102 seconds of write
/// hold**, during which every keystroke, hover and completion in the editor waited. The queue was empty when the
/// pump got control back, which is why its own line says `0 left`.
///
/// **The number is larger than the pump's own slice on purpose.** A file cannot be cooked until its include chain
/// has been read, so a pull that stops too early cannot answer at all — and an empty answer is the defect this
/// function exists to fix. The pump finishes whatever is left within the second, and the pull no longer holds the
/// editor for a minute and a half.
///
/// **Twenty-four, and it was five hundred** — a number about a *project*, applied to a queue that is not the file's
/// include chain: while a module was being read the queue held the standard library's four hundred files, and every
/// pull drained them. Measured on `test_module` with `src/std_module_demo.cpp` open (`import std;`):
///
/// ```text
///                                              500        24
/// first pull after opening the file            1.95 s     271 ms
/// longest update hold in the session           1 815 ms   86 ms
/// the completion that follows                  153 ms     30 ms
/// ```
const PREPARE_INDEX_SLICE: usize = 24;

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
    /// **The last report this server gave each file** — see [`AnalysisState::last_report`].
    ///
    /// It exists because a pull has no way to say "I do not know yet". A file whose cooked reading is gone (an
    /// edit) answers `None` from the analysis, and `None` becomes an **empty report**, which a client reads as "no
    /// problems" and clears the squiggles it is showing. The next pump slice cooks the file and
    /// `workspace/diagnostic/refresh` makes the client ask again — and the squiggles come back. Every keystroke
    /// repeats it: that is the "diagnostics flash" a user sees, and no amount of refresh taming fixes it, because
    /// the answer itself alternates.
    last_reports: std::sync::Mutex<
        std::collections::HashMap<PathBuf, std::sync::Arc<Vec<lsp_types::Diagnostic>>>,
    >,
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
            last_reports: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// **The report this server last gave for `path`**, if it gave one — see [`AnalysisState::last_reports`].
    ///
    /// A pull answers from the analysis when it can and from this when it cannot, so that a client is never told
    /// "and now there is nothing" by a server that simply has not finished reading. The alternative for the caller
    /// is an empty report, which is a *claim*: it says the file is clean.
    pub fn last_report(
        &self,
        path: &std::path::Path,
    ) -> Option<std::sync::Arc<Vec<lsp_types::Diagnostic>>> {
        self.last_reports
            .lock()
            .ok()
            .and_then(|reports| reports.get(path).map(std::sync::Arc::clone))
    }

    /// **Remember what was just sent**, so that the next pull that cannot answer keeps showing it.
    ///
    /// Bounded to the open documents' worth of files by the call site (`didClose` forgets one), so it is not a
    /// project-sized cache: it holds what a client is currently looking at.
    pub fn remember_report(
        &self,
        path: std::path::PathBuf,
        items: std::sync::Arc<Vec<lsp_types::Diagnostic>>,
    ) {
        if let Ok(mut reports) = self.last_reports.lock() {
            reports.insert(path, items);
        }
    }

    /// **Forget one file's last report** — a close, or a workspace reload.
    pub fn forget_report(&self, path: &std::path::Path) {
        if let Ok(mut reports) = self.last_reports.lock() {
            reports.remove(path);
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
        //
        // **A hypothesis tried here and withdrawn, with its measurement.** The shortcut's condition is "the file is
        // held and its summary is current", which is *not* the same as "its rendering exists" — so it looked like the
        // reason a file the client opened never got cooked, and that in turn looked like the reason several handshake
        // tests report `"result":null` on every retry for thirty seconds. Adding `&& session.view(path).is_some()` to
        // the condition changed the handshake result **not at all** (6 passed, 17 failed, before and after), so the
        // hypothesis is wrong — and the condition costs a `view()` call on the hot path the shortcut exists for.
        // Withdrawn rather than kept as a plausible-looking change that nothing measured.

        // **The shortcut those paragraphs describe — and which was not here to take.**
        //
        // Everything above is the argument for not asking for the write lock, and the function then asked for it
        // unconditionally: `update_session` is a *write*, and the pump holds the session for reading for a slice (50 ms
        // is the budget line, and the log warns when a request holds the lock that long). Measured on
        // `EmmyLuaCodeStyle` with `CPPLS_TRACE_CODE_ACTION=1`: `codeAction` took **56 ms, of which `prepare` was
        // 54 ms** — and the fix provider itself was **14 µs**. The handler was not slow; it was queueing.
        //
        // The condition is the one the doc comments argue for: **a rendering already exists**, so `load` has nothing to
        // read and `want_cooked_reading` has nothing to request. `view` is a read, and a read does not wait for the
        // pump's parse.
        if self
            .with_snapshot(|session| session.view(path).is_some())
            .unwrap_or(false)
        {
            return true;
        }

        let path = path.to_path_buf();
        let queued = self
            .update_session("prepare a file a request named", move |session| {
                let loaded = session.load(&path).is_some();
                // **A file a request is about gets its cooked reading** — and this is the only place that knows which
                // file a request named. Not "cook it now": the query below answers from the reading that exists, and
                // the *next* request gets the compiler's reading, which is what `isIncomplete` tells a client to come
                // back for. The session's rule is that a reading is built when something looks at the file; this is
                // the "something" for everything the editor did not open.
                if loaded {
                    session.want_cooked_reading(&path);
                }
                loaded
            })
            .await
            .unwrap_or(false);

        // **And the consumer is woken, which it was not.**
        //
        // `want_cooked_reading` puts a file in the cooking queue; the queue is a value, and something has to *look*
        // at it. The pump is woken by the notification paths (`didOpen`, `didChange`, a watched file) and by
        // nothing else — so a file this function queued sat in the queue until the reader happened to type, and the
        // request that was supposed to be answered "next time" was answered `None` for as long as nobody edited
        // anything. The deferral's own contract is *"the next request gets the compiler's reading"*; without this
        // wake there was no next event to get it on.
        //
        // Woken after the write above, not inside it: `wake` is a `Notify` and the queue is the message, so it
        // carries no data and cannot be lost by arriving early — which is the arrangement that comment describes.
        if queued {
            self.wake();
        }
        queued
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

    /// **Read one whole program — the one part of a slice whose cost must not be paid under any lock.**
    ///
    /// A program read is a walk, a cook of every file in include order, a parse of the whole program and a sweep of
    /// every declaration in it: **5.09 s** for a 159-file, 7.07 MB program, and 63 ms for the same program from the
    /// database. Of that, the walk and the closure's texts are **44 ms** and they are the only part that needs the
    /// session — so the three phases are priced completely differently, and this is where they are taken apart:
    ///
    /// ```text
    /// Session::gather_a_unit_read      under the read lock   44 ms    the walk, the texts, two cache reads
    /// UnitReadMaterials::read_the_program   NO LOCK         4.9 s     the cook, the parse, the sweep
    /// Session::commit_a_program        under the write lock a few ms  one insert_cooked per file
    /// ```
    ///
    /// # Why the middle phase runs on a blocking thread of its own
    ///
    /// Because it is the session's **only** indivisible piece of work, and the walk's 44 ms is the only part that has
    /// to hold the lock a keystroke waits for. Measured over the wire, before the split: a unit read inside the
    /// writer held it for **all of it**, which is why the capability sat unused for several rounds
    /// (`docs/architecture.md` §7.4). Nothing about it needs an `async` context, so it must not occupy an `async`
    /// one: a 4.9 second task on the runtime would take the executor away from every request in flight — the same
    /// reason `run_blocking` exists.
    ///
    /// Returns whether a program was attempted. `false` when none was owed, or when the clock
    /// ([`cpp_code_analysis::Session::the_program_to_read`]) or the size budget said no.
    pub async fn read_a_program(&self) -> bool {
        let Some(Some(root)) = self.with_snapshot(|session| session.the_program_to_read()) else {
            return false;
        };

        // **The cheap half under the lock that is already taken for reads**, beside whatever other queries are doing.
        let gathering = root.clone();
        let materials = self
            .run_blocking(move |session| session.gather_a_unit_read(&gathering))
            .await;

        // **The expensive half with no lock at all.** `spawn_blocking` rather than the analysis pool: this work is
        // the background's, and it must not be one of the permits a request waits for.
        let prepared = match materials {
            Some(materials) => tokio::task::spawn_blocking(move || materials.read_the_program())
                .await
                .unwrap_or(None),
            None => None,
        };

        // **And the inserts, under the writer.** The root is marked read even when the read declined, so a program
        // that cannot be built is not chosen again on the next wake.
        let read = self
            .update_session("read a program", move |session| {
                session.commit_a_program(&root, prepared)
            })
            .await
            .flatten();

        // **One line, because this is the most expensive thing the server does unprompted** — and the reader's log is
        // where "why is a core busy for five seconds after I stop typing" is answered. Silent when the *insert* is
        // all that happened (a program the database already had), which is the cheap case.
        if let Some(reading) = &read {
            log::info!(
                "read a program as one piece: {} file(s), {} token(s), {} error(s) — their declarations are the \
                 compiler's reading now",
                reading.files,
                reading.tokens,
                reading.errors
            );
        }
        read.is_some()
    }

    /// Wait for a wake-up, or for `timeout` to pass.
    ///
    /// Returns whether a wake-up arrived. The timeout is **insurance, not the mechanism**: `Notify` stores one
    /// permit, so a wake that arrives before this call is not lost — but a stall would leave the index empty for
    /// the rest of the session, and one lock acquisition a second is a cheap way never to find out.
    pub async fn wait_for_work(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.work.notified())
            .await
            .is_ok()
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

    /// **Is there a reading for this file, and read and cook it if not.**
    ///
    /// The write half of a pull request, and it must happen **before** the query: [`AnalysisState::query_blocking`]
    /// runs its closure under the read lock — that is what lets queries run beside the index pump — while reading a
    /// file and cooking it are `&mut Session`. So the two are split where the two locks are, and this is the first.
    ///
    /// # Why `textDocument/diagnostic` needed it
    ///
    /// The pull handler used [`AnalysisState::prepare`], which **queues** a file for cooking and returns — and then
    /// asked the query, which found no reading and answered `Missing`. A `Missing` outcome becomes a JSON `null`
    /// response (`context::ServerContext::task`), and a client expecting a `DocumentDiagnosticReport` drops it: the
    /// diagnostics it already had stay on screen, which is what "输入代码之后诊断不改变" is.
    ///
    /// The `bool` is whether the file can be cooked at all: `false` means it is not there, which is the honest
    /// `Missing` rather than an empty report.
    ///
    /// # The fast path, and the 195 seconds that made it necessary
    ///
    /// Every one of those three steps is **discovery work that has already been done**, and there was no check for
    /// that. `add_project_files` re-queues **every file of the project** — see its own note, where the queue is the
    /// one thing it always touches — and `index_everything` then drains that queue, so a pull request re-indexed the
    /// whole project. A write hold is what every other request waits for, so this is not "diagnostics are slow": it
    /// is **hover, completion and every keystroke** waiting behind it.
    ///
    /// Measured on a real project (`EmmyLuaCodeStyle`, 5 000+ files) from the server's own budget warning, which
    /// names the label this function passes:
    ///
    /// ```text
    /// 17:52:25 `prepare a file a pull request named` held the analysis for 195264 ms
    /// 17:53:58   … 3524 ms
    /// 17:54:20   … 2068 ms
    /// 17:54:49   … 7018 ms
    /// ```
    ///
    /// **195 seconds, and then seconds on every request after it.**
    ///
    /// # Why the guard is these two questions, and why one was not enough
    ///
    /// **`session.view(path)` is `Some` only when the file is indexed *and* its rendering is cached.** It builds
    /// nothing; on a miss it only notes that the file wants cooking. So it is exactly "the rendering this function
    /// exists to produce is already there".
    ///
    /// **And it is not enough on its own**, which a test caught: a *rendering* is cached by content hash and an
    /// **edit does not evict it** — the file's summary is what an edit drops (see [`crate::FileSummary`] and
    /// `Session::did_open`). A guard of `view` alone therefore answered "already prepared" for a buffer the client
    /// had just changed, skipped the re-index, and the query that followed found no summary and answered nothing —
    /// which is the very defect (`诊断不改变`) this function was written to fix. Measured by
    /// `diagnostics_are_answered_before_the_index_has_read_the_file`, which indexes a file, edits it, and requires
    /// the pull path to bring the answer back.
    ///
    /// So the summary is required as well: **a summary exists exactly when the index has read the file's current
    /// text**, and that is the freshness this has to be sure of. Both together mean "the work is done and it is the
    /// work for the text now in the buffer".
    ///
    /// It is also conservative in the direction that matters: a file that is **not** ready still takes the path
    /// below unchanged, so a cold file, an edited file and a file whose rendering is one pump away all behave
    /// exactly as they did. Nothing here can make a diagnostic disappear that used to appear; the only change is
    /// that work already finished is not done again.
    pub async fn prepare_for_a_diagnostic(&self, path: &std::path::Path) -> bool {
        let to_prepare = path.to_path_buf();

        // **`try_read`, not `with_snapshot`.** A blocking read here would queue this request behind whatever write
        // hold is in flight — which during a cold index is the 195 seconds this whole guard exists to avoid. An
        // unavailable lock means "cannot answer the question yet", which is a `false` that falls through to the
        // path below rather than a wait.
        //
        // **What counts as ready is the reading, not the summary.** It used to require `session.view(…)` as well —
        // a *parse of the file* — and requiring one to decide whether there is work to do is the wrong question on
        // a path whose purpose is to be free when there is none. But the summary alone is the other wrong answer:
        // a file can be **read** and not yet **cooked**, and `Session::diagnostics` answers from the cooked reading —
        // so a pull that treated "read" as "ready" skipped the cook and answered from the coarser raw reading.
        // Measured: `a_pulling_client_gets_a_report_and_not_null` and `an_error_typed_into_an_open_file_is_answered`
        // both failed on an unclosed class (a report with no items is "clean" and wrong) once `didOpen` began
        // reading the file before the handler returned. A cooked reading is the honest test — it is exactly the
        // state in which this function has nothing left to do.
        if let Ok(session) = self.inner.try_read()
            && let Some(session) = session.as_ref()
            && session.index().summary(&to_prepare).is_some()
            && session.index().cooked_declarations(&to_prepare).is_some()
        {
            return true;
        }

        self.update_session("prepare a file a pull request named", move |session| {
            // **Already cooked: nothing to do, even though the read lock was unavailable a moment ago.**
            //
            // The fast path above needs the *read* lock, and the pump holds the write lock in slices whenever there
            // is work — so on a busy session a pull that would need no work at all fell through to this closure,
            // loaded the file, drained the queue and cooked it **again**. Cooking a system header is a macro
            // expansion of a hundred kilobytes: the header of a real standard-library file costs tens of
            // milliseconds, which is why opening `<ostream>` turned into a core at 100 %, with the diagnostics
            // flashing because every pull re-queued work that the next pull then waited on.
            //
            // Asking the question *inside* the write lock costs nothing and settles it: a view that is already
            // there means this request has all it needs.
            if session.index().summary(&to_prepare).is_some() && session.view(&to_prepare).is_some()
            {
                return true;
            }

            session.load(&to_prepare);

            // **`index_everything`, but `add_project_files` only for a file the index has never heard of.**
            //
            // The pair is the whole of the cost, and only one half of it is needed for the ordinary case.
            // `add_project_files` re-queues **every file of the project** — it is a discovery step, and its own note
            // says the queue is the one thing it always touches — so a pull re-indexed a 5 000-file project to
            // answer a question about one buffer. `index_everything` is the *drain*, and what it drains can be just
            // this file.
            //
            // And for an edited file it already is: `Session::did_change`/`did_open` queue it at `Priority::Open`
            // and mark it for cooking (see `Session::buffer_changed`). So the file being prepared is already the
            // head of the queue and the drain below reads **it** — which is exactly the "answer before the pump
            // reaches it" that the pull path exists for. Dropping the re-queue removes the project-wide work and
            // changes nothing about which file gets read.
            //
            // Measured from the budget warning before this, on every keystroke rather than once:
            //
            // ```text
            // 17:53:58 `prepare a file a pull request named` held the analysis for 3524 ms
            // 17:54:20   … 2068 ms
            // 17:54:49   … 7018 ms
            // ```
            // **Discovery is for a file the session has not been told about yet**, and the test for that is the
            // project list rather than the file's location: a buffer the client opened from outside the folder (a
            // header `Go to definition` reached, an untitled buffer) still has to be queued once before it can be
            // cooked, and a system header a reader opens is *one* such file, not one per pull — the in-lock check
            // above is what keeps the second pull out of here entirely.
            if !session
                .project_files()
                .iter()
                .any(|known| known == &to_prepare)
            {
                session.add_project_files([to_prepare.clone()]);
            }

            // **Bounded, and by a slice.** That call loops until the queue is empty, so a pull was asking for the
            // whole project — see [`PREPARE_INDEX_SLICE`] for the 102 seconds that cost. A pull is a nudge for one
            // file; the pump is the loop that owns this work, and anything still queued is left for it.
            //
            // **A count and not a deadline, which was tried here and is worse.** The file cannot be cooked until
            // its include chain has been read, and how many files that is cannot be known without reading them — so
            // a slice that is too small answers `false` and the client shows nothing, which is the defect this
            // function exists to fix. A count large enough for an ordinary include chain and far short of a project
            // is the honest setting; the pump finishes the rest within the second.
            session.advance(PREPARE_INDEX_SLICE);
            session.cook(&to_prepare).is_some()
        })
        .await
        .unwrap_or(false)
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
        self.worst_write_hold
            .load(std::sync::atomic::Ordering::Relaxed)
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

    /// **Diagnostics for a file nobody has indexed are still an answer.**
    ///
    /// This is the defect as a contract, with no pump and no clock in it. A diagnostic is answered from the file's
    /// **cooked reading**, and an edit drops that reading — so the pass that runs after typing stops would find none
    /// and publish nothing, however long the user waited. Measured on this shape:
    ///
    /// ```text
    /// before the edit:       Some(1)
    /// right after the edit:  None       ← what the client was told, as nothing at all
    /// after re-indexing:     Some(1)
    /// ```
    ///
    /// The file here is a buffer with no summary and nothing having cooked it, which is exactly the state a file is
    /// in between an edit and the index pump reaching it. A handler that only *reads* answers `None` — and that
    /// `None` reached the client as a JSON `null`; one that cooks the file's current text answers, which is what
    /// makes the pull path independent of the pump's queue.
    #[tokio::test]
    async fn diagnostics_are_answered_before_the_index_has_read_the_file() {
        let state = AnalysisState::new();
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;

        // A missing `;`, which is a parse error and nothing else.
        let buffer = "int g();\nstruct Name {\n    int middleName;\n    void print(int i) {\n        middleName = i\n    }\n};\n";
        state.files().overlay.open("/p/main.cpp", buffer);
        // **Indexed, then edited** — the sequence the defect lives in. A file the client has open is one the index
        // read, so its reading existed; the edit is what drops it, and the pump is what would bring it back (later
        // than the typing, in a project of any size).
        state
            .update_session("a test's index pass", move |session| {
                session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
                session.index_everything();
                session.did_open(std::path::Path::new("/p/main.cpp"), buffer);
                session
                    .diagnostics(std::path::Path::new("/p/main.cpp"))
                    .map(|found| found.errors.len())
            })
            .await;
        // **The state the pass runs in.** Not an assertion — it documents the measurement the fix rests on: after an
        // edit the file has no reading, and this is what the client used to be told, as nothing at all.
        let after_the_edit = state
            .with_snapshot(|session| {
                session
                    .diagnostics(std::path::Path::new("/p/main.cpp"))
                    .map(|found| found.errors.len())
            })
            .flatten();
        assert_eq!(
            after_the_edit, None,
            "an edit drops the file's summary, so a pass that only reads has no answer — which is the defect"
        );

        // **The pull path's own two steps**, which is what a `textDocument/diagnostic` request is made of.
        assert!(
            state
                .prepare_for_a_diagnostic(std::path::Path::new("/p/main.cpp"))
                .await,
            "the file can be read and cooked"
        );
        let diagnostics = state
            .with_snapshot(|session| {
                crate::handlers::diagnostic::diagnose_file(
                    session,
                    std::path::Path::new("/p/main.cpp"),
                )
            })
            .flatten()
            .expect("a file whose text the client is holding has an answer")
            .1;
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(';')),
            "the missing `;` is the only thing wrong with the buffer: {:?}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
        );
    }

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

    /// **A second pull for a file already analysed must not re-index the project.**
    ///
    /// This is the defect the guard exists for, measured on a real project from the server's own budget warning:
    ///
    /// ```text
    /// 17:52:25 `prepare a file a pull request named` held the analysis for 195264 ms
    /// 17:53:58   … 3524 ms
    /// 17:54:20   … 2068 ms
    /// 17:54:49   … 7018 ms
    /// ```
    ///
    /// Every one of those is `add_project_files` re-queuing **every file of the project** and `index_everything`
    /// draining it again, for a file whose answer was already sitting in the index. A write hold is what every other
    /// request waits for, so the cost was paid by hover, completion and typing rather than by diagnostics.
    ///
    /// # How the fast path is detected rather than assumed
    ///
    /// Not by timing, which is flaky, and not by the queue, which was measured and does not work: `add_project_files`
    /// refills the index queue and `index_everything` drains it **inside the same write hold**, so a caller outside
    /// that hold sees zero either way. What *is* observable is the thing that made the cost expensive — **taking the
    /// write lock at all**. So the test holds the analysis's own gate for writing and then pulls: a pull that needs
    /// the lock cannot return, and one that finds the work already done answers without it.
    ///
    /// That is also the shape of the defect: the write hold is what every other request waits for, so a pull that
    /// takes it is a pull that delays hover, completion and typing.
    /// **A pull leaves the queue for the pump**, which is the 102 seconds this test exists to prevent.
    ///
    /// `Session::index_everything` loops until the queue is empty, so asking for it asks for the whole project — and
    /// a pull request did. Measured in the server's own log while this was being fixed, with the pump 1 258 files
    /// from finished:
    ///
    /// ```text
    /// 20:24:05 `prepare a file a pull request named` held the analysis for 102700 ms
    /// 20:24:09 index slice: prepared 1, committed 1, write lock held 0 ms, 1 to cook, 0 left
    /// ```
    ///
    /// The assertion is on what the pull **left behind**, because that is the property: a bounded pull hands the
    /// rest of the project back to the loop that owns it. Both directions are covered — this fails if the bound is
    /// removed (zero left), and the cold-start tests next door fail if it is made too small to read a file's
    /// includes.
    #[tokio::test]
    async fn a_pull_leaves_the_queue_for_the_pump() {
        let state = AnalysisState::new();
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;

        // A project far larger than one pull's slice.
        state
            .files()
            .overlay
            .open("/p/main.cpp", "int main() { return 0; }\n");
        let many: Vec<PathBuf> = (0..1500)
            .map(|at| PathBuf::from(format!("/p/file{at}.cpp")))
            .collect();
        for path in &many {
            state.files().overlay.open(
                path.to_str().unwrap_or_default(),
                "int a_variable_in_this_file = 0;\n",
            );
        }
        state
            .update_session("the fixture's discovery", move |session| {
                session.add_project_files(many);
            })
            .await;

        state
            .prepare_for_a_diagnostic(std::path::Path::new("/p/main.cpp"))
            .await;

        let left = state
            .with_snapshot(|session| session.pending())
            .unwrap_or(usize::MAX);
        assert!(
            left > 0,
            "a pull must not drain the whole project — that was 102 seconds of write hold, and every keystroke, \
             hover and completion in the editor waited for it"
        );
    }

    /// **A second pull for a file already analysed must not re-index the project.**
    ///
    /// This is the defect the guard exists for, measured on a real project from the server's own budget warning:
    ///
    /// ```text
    /// 17:52:25 `prepare a file a pull request named` held the analysis for 195264 ms
    /// 17:53:58   … 3524 ms
    /// 17:54:20   … 2068 ms
    /// 17:54:49   … 7018 ms
    /// ```
    ///
    /// Every one of those is discovery work that had already been done, for a file whose answer was already in the
    /// index. A write hold is what every other request waits for, so the cost was paid by hover, completion and
    /// typing rather than by diagnostics.
    ///
    /// # How the fast path is detected rather than assumed
    ///
    /// Not by timing, which is flaky, and not by the queue, which was measured and does not work: `add_project_files`
    /// refills the index queue and the drain empties it **inside the same write hold**, so a caller outside that hold
    /// sees zero either way. What *is* observable is the thing that made the cost expensive — **taking the write lock
    /// at all**. So the test holds the analysis's own gate for writing and then pulls: a pull that needs the lock
    /// cannot return, and one that finds the work already done answers without it.
    ///
    /// That is also the shape of the defect: the write hold is what every other request waits for, so a pull that
    /// takes it is a pull that delays hover, completion and typing.
    #[tokio::test]
    async fn a_second_pull_does_not_take_the_analysis_for_writing() {
        let state = AnalysisState::new();
        let root = std::env::temp_dir().join("cppls-analysis-state-tests");
        opened(&state, &root).await;

        state
            .files()
            .overlay
            .open("/p/main.cpp", "int main() { return 0; }\n");
        let path = std::path::Path::new("/p/main.cpp");

        // **The first pull does the work**, and it is the file's first: the cold path, unchanged.
        assert!(
            state.prepare_for_a_diagnostic(path).await,
            "the file can be read and cooked"
        );
        state
            .update_session("a test's drain", |session| session.index_everything())
            .await;
        assert!(
            state
                .with_snapshot(|session| session.view(path).is_some())
                .unwrap_or(false),
            "the fixture must reach the analysed state, or this test proves nothing about skipping it"
        );

        // **Hold the gate for writing.** Any `update_session` — which is what the slow path is made of — blocks
        // here, so a pull that returns has proved it never asked for the lock.
        let held = state.gate.clone().write_owned().await;
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            state.prepare_for_a_diagnostic(path),
        )
        .await;
        drop(held);

        assert_eq!(
            second,
            Ok(true),
            "a file already indexed and cooked is prepared from the read lock alone; a pull that needed the write \
             lock would still be waiting here — and that wait is what every other request pays"
        );
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
            // **The text the analysis reads, asked of the file table rather than of the rendering.** `Session::view` is a
            // request for the *cooked* reading and answers `None` until that work has run — a different claim ("the
            // rendering is ready") from this test's ("the buffer is the text the session holds"). Measured: the assertion
            // read `None` against `Some("int from_the_handle;\n")`.
            .with_snapshot(|session| {
                session
                    .files()
                    .held("/p/main.cpp")
                    .map(|file| file.text.clone())
            })
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
            // **The text the analysis reads, asked of the file table rather than of the rendering.** `Session::view` is a
            // request for the *cooked* reading and answers `None` until that work has run — a different claim ("the
            // rendering is ready") from this test's ("the buffer is the text the session holds"). Measured: the assertion
            // read `None` against `Some("int from_the_handle;\n")`.
            .with_snapshot(|session| {
                session
                    .files()
                    .held("/p/main.cpp")
                    .map(|file| file.text.clone())
            })
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
