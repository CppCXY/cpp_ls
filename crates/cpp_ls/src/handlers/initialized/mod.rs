//! # `initialized` — where the workspace becomes an analysis
//!
//! The handshake has two halves and they arrive at different times, which is why this module exists at all:
//!
//! ```text
//! initialize    the client's capabilities and its clientInfo — the server, and nothing about a project
//! initialized   the workspace folders — and *this* is what a session is opened over
//! ```
//!
//! So the work here is the order the pieces depend on each other:
//!
//! ```text
//! 1. roots            from the params, with `rootUri` as the fallback a client still sends
//! 2. logger           needs a root, because the log lives under the workspace
//! 3. client config    what the client says is not source (`workspace/configuration`)
//! 4. the session      opened over the first root: the compiler is run, `compile_commands.json` is read
//! 5. the buffers      a client restarting sends its open documents *after* this, but a reload has them already
//! 6. indexing         in the background, so the first request is answered while the project is still being read
//! 7. file watches     client registration or a native fallback
//! 8. diagnostics      push scheduling or pull refresh, once the index holds something
//! ```
//!
//! Steps 6 to 8 are what make this a *language server* rather than a query engine: the session is usable the moment
//! step 4 returns, and everything after it makes the answers better without making the editor wait.

use crate::context;
use crate::context::LoadIndicator;
use crate::handlers;
mod client_config;

use std::path::PathBuf;

use lsp_types::InitializeParams;

use crate::cmd_args::CmdArgs;
use crate::context::{ProgressTask, ServerContextSnapshot};
use crate::handlers::register_files_watch;
use crate::logger::init_logger;
use crate::util::uri_to_file_path;

pub use client_config::{ClientConfig, get_client_config};

/// How long the pump sleeps when there is nothing to read, before looking again.
///
/// A safety net rather than the mechanism: an edit *wakes* the pump (`AnalysisState::wake`), and a wake-up that
/// arrives while the pump is busy is remembered. This is the backstop for one arriving in the instant between the
/// queue being found empty and the wait starting — one lock acquisition a second, against an index that would
/// otherwise stay stale for the rest of the session if that ever happened.
const IDLE_WAIT: std::time::Duration = std::time::Duration::from_millis(1000);

/// Maximum number of files prepared in one adaptive background wave.
const INDEX_SLICE: usize = 24;

/// **How many cooked readings one insert holds the writer for.** The claim is wider than this (see
/// [`COOK_A_SLICE`]) because the decode and the render run without the lock and parallelise over it; the insert takes
/// the writer, so it is cut to what a request can wait for.
const COMMIT_A_SLICE: usize = 4;

/// Maximum number of cooks gathered per slice; rendering runs without the session lock.
/// **How many files one cooking slice claims.**
///
/// Four was the number before the claim's stored-record decode was learned to be the drain's cost. A slice decodes one
/// rendering per claimed file (~100 ms each) and renders one closure per file it has no record for, so a claim of four
/// left the chunked decode four files wide while the rest of the slice paid its own overhead thirteen times more often
/// than it had to.
///
/// Measured on neovim's `ui.c`, cold cache, same machine:
///
/// ```text
/// claim   index   drain tails   worst write-lock hold
///     4    53 s           45          931 ms
///    16    43 s           12          ~1 s
///    32    39 s            6         3165 ms   ← refused: the insert holds the writer for three seconds
/// ```
///
/// Sixteen is the middle: it spreads the decode over the cores without making one insert a stall a request can feel.
const COOK_A_SLICE: usize = 16;

/// **How much of a drain one read hold is allowed to do.**
///
/// The drain was unbounded — every file the wave left and every environment its slice named — and it runs under the
/// session's read lock. Measured on neovim's `main.c`: one drain held the lock for **1842 ms**, and the writer's
/// twenty-millisecond cook queued behind it while every request queued behind the writer. The leftovers go back on
/// the queues ([`Session::commit_the_drain`] does it) and the next slice takes them.
const DRAIN_FILES_A_HOLD: usize = 1;

/// How many files' macro environments one read hold builds — see [`DRAIN_FILES_A_HOLD`]. Each is a closure walk, so
/// the bound is wider than the re-read's and still one slice's worth.
const DRAIN_ENVIRONMENTS_A_HOLD: usize = 4;

/// **How many stored cook records one read hold decodes** — see [`DRAIN_FILES_A_HOLD`] for why the slice is wider than
/// the hold. Each record is a file's whole closure's text, measured at ~100 ms, so four is what a request can wait for.
const COOKS_A_HOLD: usize = 1;

/// Feedback target for adjusting wave size, not a hard end-to-end latency deadline.
const INDEX_SLICE_MS: u64 = 30;

pub async fn initialized_handler(
    context: ServerContextSnapshot,
    params: InitializeParams,
    cmd_args: CmdArgs,
) -> Option<()> {
    let roots = workspace_roots(&params);
    let log_file = init_logger(roots.first().and_then(|root| root.to_str()), &cmd_args);

    // **The session's identity, not a second computation of it** — see `the_effective_client_id`, and the capability
    // side that reads the same value (`docs/worklog.md` P2-2).
    let client_id = context.lsp_features().client_id();
    let supports_config_request = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.configuration)
        .unwrap_or_default();
    log::info!("client {client_id:?}, workspace roots {roots:?}");

    // **The workspace is installed before anything is asked about it.** `workspace/configuration` takes its scope from
    // the roots — a folder-scoped setting is asked for under a `scopeUri`, and what a client answers without one is its
    // *global* setting, which is a different thing from the folder's — so the roots have to exist before the ask
    // (`docs/worklog.md` P1-1). Measured: the request went out as
    // `{"items": [{"section": "cppls"}]}` with no `scopeUri` at all.
    //
    // Nothing here needs the session: setting the roots, the log file and the `--config` path are three plain fields,
    // which is why this ordering costs nothing.
    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.set_roots(roots.clone());
        workspace_manager.set_log_file(log_file);
        // The `--config` a caller named is kept, because a reload re-opens the session and has no `CmdArgs` of its
        // own: the file the user pointed at is part of what the workspace *is*.
        workspace_manager.set_config_file(cmd_args.config.0.clone().map(PathBuf::from));
    }

    // The client's configuration is read **before** the session is opened, because one of the things it says —
    // which files are not the project's — is part of the filter the session is opened with.
    let client_config = get_client_config(&context, client_id, supports_config_request).await;
    log::info!("client config: {client_config:?}");
    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.set_client_config(client_config);
    }

    // **The watch is registered before the analysis starts**, because whether the client took it decides whether this
    // server watches the project itself — and `start_analysis` is where that decision is made
    // (`docs/worklog.md` P2-3). On a reload the order is the same: `start_analysis` runs again and reads
    // the answer the previous registration left.
    register_files_watch(context.clone()).await;
    start_analysis(context.clone()).await;

    // **Debounced persistence, one background writer at a time.** Editing only marks a file; this task wakes after
    // two seconds and writes at most what has been quiet, under the read side of the analysis.
    let persistence = context.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            persistence.analysis().persist_cooked().await;
        }
    });

    Some(())
}

/// Open the analysis over the workspace and start reading it.
///
/// Called once at `initialized` and again on every workspace reload (`WorkspaceManager`), which is why it takes the
/// context and asks the state where the workspace is rather than being handed a root.
pub async fn start_analysis(context: ServerContextSnapshot) {
    let Some((root, version, load)) = open_session(&context).await else {
        return;
    };

    if context.workspace_manager().lock().await.is_closing() {
        return;
    }

    // **The client's answer, not only its advertisement.** A client that advertises dynamic registration and then
    // refuses the registration has no watcher, so this server starts one; `the_client_watches` is `None` when no
    // registration was attempted (a client that never said it could watch), and `Some(true)` when it took the watch.
    let the_client_watches = {
        let workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.the_client_watches()
    };
    let the_client_watches = context
        .lsp_features()
        .supports_dynamic_watched_files_registration()
        && the_client_watches != Some(false);
    if !the_client_watches {
        let log_file = context
            .workspace_manager()
            .lock()
            .await
            .log_file()
            .map(PathBuf::from);
        if let Some(mut filter) = context
            .analysis()
            .with_snapshot(|session| session.watch_filter())
        {
            if let Some(log_file) = log_file {
                match std::fs::canonicalize(&log_file) {
                    Ok(path) => filter = filter.ignore(path),
                    Err(error) => log::warn!(
                        "cannot exclude the server log {} from the watcher: {error}",
                        log_file.display()
                    ),
                }
            }
            // **`CPPLS_NO_WATCH` is an A/B switch, not a setting**: with it set no OS watcher runs, which is how a
            // run can tell whether the watcher's own loop is what burns a core and grows the process. The project's
            // own answer is `[workspace] native_watch = false`, which is a decision rather than a probe — a checkout
            // whose events are noise, or a reader who would rather trust the editor.
            let watching_off = context
                .analysis()
                .with_snapshot(|session| {
                    session
                        .project_config()
                        .config
                        .workspace
                        .native_watch
                        .is_some_and(|wanted| !wanted)
                })
                .unwrap_or(false);
            if watching_off {
                log::info!(
                    "the native watcher is off: the project's own configuration says \
                     `[workspace] native_watch = false`"
                );
            } else if let Err(error) = context
                .native_watcher()
                .start(
                    context.clone(),
                    root.clone(),
                    filter,
                    context
                        .workspace_manager()
                        .lock()
                        .await
                        .configuration_outside_the_root()
                        .into_iter()
                        .collect(),
                )
                .await
            {
                context.native_watcher().stop().await;
                let message = format!(
                    "Cannot watch workspace files: {error}. External changes may not be detected."
                );
                log::error!("{message}");
                context.client().show_message(lsp_types::ShowMessageParams {
                    typ: lsp_types::MessageType::WARNING,
                    message,
                });
            }
        }
    }

    if context.workspace_manager().lock().await.is_closing() {
        context.native_watcher().stop().await;
        return;
    }

    context.status_bar().update_progress_task(
        ProgressTask::LoadWorkspace,
        None,
        Some(format!("Indexing {}", root.display())),
    );

    index_in_background(context.clone(), version, load).await;
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;
}

/// Open a session over the workspace root, with the client's buffers already in front of it.
///
/// `None` when the client has named no folder: there is nothing to open over, and every query answers "no analysis"
/// until it does — which is the honest state rather than an empty project.
async fn open_session(context: &ServerContextSnapshot) -> Option<(PathBuf, i64, LoadIndicator)> {
    let (root, filter, config_file, version, generation) = {
        let workspace_manager = context.workspace_manager().lock().await;
        if workspace_manager.is_closing() {
            return None;
        }
        (
            workspace_manager.root()?.to_path_buf(),
            workspace_manager.watch_filter()?,
            workspace_manager.config_file().map(PathBuf::from),
            workspace_manager.workspace_version(),
            workspace_manager.reload_generation(),
        )
    };

    // **The bar is a guard, not a pair of calls.** It ends when this value is dropped, whichever way the load
    // stops: a session superseded while it was being built (the `return None` below), a pump that returns because
    // the workspace was reloaded, or a panic. Both of the first two used to leave the reader's bar frozen at its
    // last report with the server idle.
    let load = context.status_bar().begin_load();

    let session = context
        .analysis()
        .build_session(root.clone(), filter, config_file)
        .await?;
    if !install_prepared_session(context, session, version, generation).await {
        log::debug!("discarding a workspace session superseded while it was being built");
        return None;
    }

    let diagnostic_delay = context
        .analysis()
        .with_snapshot(|session| session.project_config().config.diagnostics.on_change_ms)
        .flatten();
    context.diagnostics().set_change_delay(diagnostic_delay);
    report_project_configuration(context);
    report_a_missing_formatter(context);

    log::info!("analysing {}", root.display());
    Some((root, version, load))
}

async fn install_prepared_session(
    context: &ServerContextSnapshot,
    mut session: cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>,
    version: i64,
    generation: u64,
) -> bool {
    let workspace_manager = context.workspace_manager().lock().await;
    if workspace_manager.is_closing()
        || workspace_manager.workspace_version() != version
        || workspace_manager.reload_generation() != generation
    {
        return false;
    }

    let open_files = workspace_manager.workspace_open_files();
    for (uri, text) in &open_files {
        if let Some(path) = uri_to_file_path(uri) {
            session.did_open(path, text);
        }
    }
    context.analysis().install_session(session).await;
    if !open_files.is_empty() {
        log::info!(
            "re-marked {} open buffers as the text the analysis reads",
            open_files.len()
        );
    }
    true
}

/// **Tell the reader, once, that the formatter `[format] formatter` names is not on this machine.**
///
/// Asked at startup rather than only when a format is requested, because the answer changes what the reader should
/// do — install clang-format, or change the setting — and a warning that first appears when they press Ctrl+S arrives
/// while they are watching the file be saved. The request path checks again (a program can disappear) and falls back
/// to the built-in formatter either way; [`crate::handlers::formatting::formatter::warn_once`] is what keeps the two
/// from saying it twice.
///
/// The built-in formatter is not looked for: it *is* this binary.
fn report_a_missing_formatter(context: &ServerContextSnapshot) {
    use crate::handlers::formatting::formatter::{Formatter, is_available, warn_once};

    let configured = context
        .analysis()
        .with_snapshot(|session| {
            session
                .project_config()
                .config
                .format
                .formatter()
                .to_string()
        })
        .unwrap_or_else(|| cpp_code_analysis::DEFAULT_FORMATTER.to_string());

    let Formatter::Program(program) = Formatter::of(&configured) else {
        return;
    };
    if is_available(&program) {
        log::info!("formatter: {program}");
        return;
    }
    if !warn_once(&program) {
        return;
    }
    let message = format!(
        "format.formatter names {program:?}, which is not on this machine. The built-in cpp_formatter is used \
         instead — install clang-format, or set formatter = \"cpp_formatter\" in .cppls.toml to choose it \
         deliberately."
    );
    log::warn!("{message}");
    context.client().show_message(lsp_types::ShowMessageParams {
        typ: lsp_types::MessageType::WARNING,
        message,
    });
}

/// Tell the user what working the project out produced, when there is something to tell.
///
/// A project with no configuration file and nothing unusual to report is silent, because that is the ordinary
/// case. Everything else is logged in full — the configuration file's problems and the discovery's (a database
/// found in a build directory, a CMake project that never exported one) — and an **error** is also shown to the
/// client: it means part of what the project said is not in effect, and a user who cannot see that would be
/// debugging an analysis that is quietly not theirs. Warnings stay in the log, because they are things this server
/// decided to do differently and the answer it produced is still a complete one.
fn report_project_configuration(context: &ServerContextSnapshot) {
    let report = context
        .analysis()
        .with_snapshot(|session| session.project_config().clone());
    let discovery = context
        .analysis()
        .with_snapshot(|session| session.discovery().clone());

    let Some(report) = report else {
        return;
    };

    if report.problems.is_empty() {
        if let Some(path) = &report.path {
            log::info!(
                "project configuration: {} (nothing to report)",
                path.display()
            );
        }
    } else {
        let path = report
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "(no configuration file)".to_string());
        log::warn!(
            "project configuration {} has {} problem(s):",
            path,
            report.problems.len()
        );
        for problem in &report.problems {
            log::warn!("  [{:?}] {}", problem.severity, problem.message);
        }
    }

    // The discovery's own problems: what the build system said, or did not.
    if let Some(discovery) = &discovery {
        for problem in &discovery.problems {
            log::warn!("[{:?}] {}", problem.severity, problem.message);
        }

        if let Some(database) = &discovery.database {
            log::info!(
                "compile database: {} ({}, {} entries)",
                database.path.display(),
                database.origin.words(),
                database.commands.len()
            );
        }
    }

    let errors: Vec<String> = report
        .errors()
        .map(|problem| problem.message.clone())
        .collect();

    if !errors.is_empty() {
        let path = report
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "the project configuration".to_string());

        context.client().show_message(lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::WARNING,
            message: format!("{path} could not be read in full:\n{}", errors.join("\n")),
        });
    }
}

/// Read the project — now, and again after every edit that queues work.
///
/// In the background because opening a project is not a thing an editor waits for: the session answers queries from
/// the moment it exists — each query reads what it needs, which is `Session`'s lazy indexing — and this loop is what
/// makes the *rest* of the project available: a jump into a header nobody has opened, a project-wide diagnostic
/// pass, a rename.
///
/// # Why it does not stop when the first pass ends
///
/// Because the analysis drops what an edit invalidates and *queues* the file rather than re-reading it
/// (`Session::did_open`: "the summary is dropped and the file goes to the front of the open half of the queue").
/// A server that pumped the queue only at startup would answer about an empty index for the rest of the session —
/// every query in a file the user has typed in would be "not read yet" — so the pump is what makes an edit visible
/// again, and it is one task rather than a rule every handler would have to remember.
async fn index_in_background(context: ServerContextSnapshot, version: i64, load: LoadIndicator) {
    tokio::spawn(async move {
        let mut first_pass = true;
        let mut last_diagnostic_revision = None;
        // What the last slice left to do — see the drain below, which is the moment a file's **cooked** reading
        // changes hands.
        let mut was_pending = false;
        // **How many slices in a row changed nothing** — see the breaker after the commit. A slice that claims a file
        // and cannot build it leaves the queue's count where it was, and a loop that keeps taking it is a pump that
        // never goes idle: measured once as `index slice: … 2 to cook, 0 left` once a second for as long as a window
        // stayed open, with every diagnostic pull answered 1.4 s late because the pump held the lock it waited for.
        let mut no_progress = 0usize;
        // **The load is not finished when the queue is empty.** The pump still writes the sharing cache, publishes
        // the first inactive regions and reads the open file's program; the bar used to end before that work, and the
        // machine stayed busy with no indicator. These flags keep it on screen until the post-drain work has run.
        let mut settling_the_load = false;

        // **The wave the pump is asking for**, adjusted after every slice (see below). It starts small because the first
        // wave of a cold start is the expensive one — measured: a fixed cap of 32 held the writer for 3309 ms on the
        // latency fixture's first wave and a completion that arrived inside it waited for all of it; a fixed 4 answered in
        // 137 ms. That is the whole reason this is a measurement rather than a constant.
        const THE_FIRST_WAVE: usize = 8;
        let mut wave = THE_FIRST_WAVE;

        // **The bar's rate** — see the report below, which is per slice.
        const PROGRESS_REPORT_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
        let mut last_report = std::time::Instant::now();
        let mut last_applied = context.inbox().applied();
        loop {
            // **Do not index while the reader is typing.**
            //
            // The pump's slices hold the session's read lock for 170–360 ms at a time (a stored record's decode was
            // 100–350 ms of that; see `read a cook slice`), the writer's `didChange` batch waits for that lock, and
            // **every request in flight waits for the writer's batch** — that is the barrier. Measured through the
            // protocol while a reader typed into `channel.c` at 80 ms: the writer waited **0.9–1.1 s** per batch and
            // completion answered in **1.3–1.6 s**, with a list three seconds stale. The index is not what the reader
            // needs in those hundred milliseconds; the writer's own cook of the edited file (22 ms) is, and it still
            // happens. So a pump that sees the client's edits arrive waits for a quiet moment instead of competing.
            const A_QUIET_MOMENT: std::time::Duration = std::time::Duration::from_millis(120);
            let applied_now = context.inbox().applied();
            if applied_now != last_applied {
                last_applied = applied_now;
                tokio::time::sleep(A_QUIET_MOMENT).await;
                continue;
            }
            // A reload bumps the version and starts a pump of its own: this one stops rather than racing it for
            // the write lock, because its idea of what the project is is the one that was replaced.
            if context.workspace_manager().lock().await.workspace_version() != version {
                log::debug!("the workspace was reloaded; this indexing pass stops");
                return;
            }

            let open_file = context
                .analysis()
                .with_snapshot(|session| session.next_open_file_to_read())
                .flatten();
            if let Some(path) = open_file
                && context.analysis().read_queued_file(&path).await
            {
                was_pending = true;
                continue;
            }

            let before = context
                .analysis()
                .with_snapshot(|session| (session.pending_work(), session.revision()));

            // **The parse, under a read lock — and the insert, under the write lock.**
            //
            // This is the split that ends the wait, rather than moving it. Reading a file into the index is a parse,
            // and a parse is a pure function of the file, the configuration and the filesystem
            // (`SummaryStore::prepare`) — it changes nothing, so it does not need the write lock that a query is
            // waiting for. `prepare_a_wave` takes `&self` for exactly that reason; `commit_a_wave` takes `&mut self`
            // and does nothing but put the summaries in the index and queue what they include.
            //
            // The two were one call (`advance`) and the numbers say what that cost: a slice of 32 files held the
            // write lock for **84, 503, 882, 1190 and 2430 ms**, and a slice of *zero* files — the cooking half — for
            // up to **1527 ms**. A completion arriving anywhere in those windows waited for all of it, which is the
            // row of latencies pinned to the two-second settle budget. Bounding the slice only moved the wait around
            // (measured: 76–356 ms slices, and the list came back **empty for eight requests** instead of five).
            // **The whole of the expensive half, under one read lock.** Two things that used to be in the writer are
            // here: the parse of the wave's files, and the **unit walk** the pass after it needs — measured holding
            // the writer for **1003 ms** on the first slice after a cold start, which was the largest single thing a
            // drain did. Neither changes anything, so neither belongs under the lock a query waits for.
            // **Sized by the number a reader can feel**, which is how long the writer is held — see [`THE_FIRST_WAVE`].
            // **The parse runs on a blocking thread, not on the runtime.** `run_blocking` is what the queries already
            // use, and the reason is the same one: a wave is a parse per file, and doing it inline in an `async` task
            // takes the executor away from every request in flight for the length of the wave. Measured with the
            // latency fixture: one completion in about twelve lost **1.7 s** with no session lock held at all — the
            // lock was free and the thread was busy. That is the difference `spawn_blocking` makes.
            let preparing = std::time::Instant::now();
            let prepared = context
                .analysis()
                .run_blocking("prepare a wave", move |session| {
                    Some(session.prepare_background_wave(wave))
                })
                .await
                .unwrap_or_default();
            let prepared_count = prepared.prepared.len();
            let prepared_in = preparing.elapsed().as_millis();
            log::trace!("pump: wave prepared ({prepared_count})");

            let Some((pending, drain, claimed, held, committed)) = context
                .analysis()
                .update_session("index slice", |session| {
                    // **Timed, and split, because the count is only a proxy for the one number a caller can feel:**
                    // how long the write lock is held. The parse is no longer in this window — it was paid for under
                    // the read lock above — and neither is the drain that follows the inserts, which comes back as a
                    // plan and is paid for under the read lock below. What is left is the insert and the queueing.
                    let started = std::time::Instant::now();
                    let (done, drain) = session.commit_a_wave(prepared, wave);
                    let taken = done.len();
                    let reused = done
                        .iter()
                        .filter(|step| step.outcome == cpp_code_analysis::StepOutcome::Reused)
                        .count();
                    let built = done
                        .iter()
                        .filter(|step| step.outcome == cpp_code_analysis::StepOutcome::Built)
                        .count();
                    let unstored = done
                        .iter()
                        .filter(|step| step.outcome == cpp_code_analysis::StepOutcome::Unstored)
                        .count();
                    let missing = done
                        .iter()
                        .filter(|step| step.outcome == cpp_code_analysis::StepOutcome::Missing)
                        .count();
                    let held = started.elapsed().as_millis();

                    // **All the work, not just the indexing queue.** A session that has read every file still has
                    // the files its open ones include to read *as a compiler reads them* (`Session::cook`), and this
                    // loop is what gives that work a caller: reading the queue alone would stop the pump one step
                    // before the declarations a reader asks about exist.
                    //
                    // **And which files the next cooking slice is for**, which is a queue pop and nothing more: what
                    // a cook needs is gathered, rendered and parsed under the read lock below, beside whatever a
                    // person is typing ([`Session::materials_for`], [`Session::render_them`]).
                    //
                    // **No cook until the index has caught up with the text.** A cook's record key is the file's
                    // *closure* — every file it includes, by content — so a cook claimed while the index still lacks
                    // a summary of some closure file computes a different key from the same cook made after the queue
                    // drains, and files its record where nobody will look. Measured on `test_cpp`: a warm run missed
                    // **72 of 75** stored cooks with `no rendering`, every one of them filed under the complete key
                    // and looked up under an incomplete one.
                    let claimed = if session.is_idle() {
                        session.claim_cooking(COOK_A_SLICE)
                    } else {
                        Vec::new()
                    };

                    let pending_now = session.pending_work();
                    // **Where the memory is, at the one moment somebody looks**: when the queue drains, the
                    // session reports what it is holding. These numbers were already collectable — `Session::cache_sizes`
                    // existed and no caller used it — and a run that reports 5 GB without them says only "it is big".
                    let sizes = session.cache_sizes();
                    // **The same numbers in a file**, because a log line is only readable if the client shows its
                    // output panel: `%TEMP%\cpp_ls-memory.txt` is one click away for anybody who reports "it uses
                    // gigabytes", and the answer to "which part" is then in hand instead of guessed at.
                    if pending_now == 0 && taken > 0 {
                        log::info!(
                            "index drained: cache {} rendering(s) ({} KB), {} translation unit(s), {} summary(ies), \\
                             {} fact(s), {} macro(s), {} cooked file(s) / {} cooked declaration(s)",
                            sizes.renderings,
                            sizes.rendering_bytes / 1024,
                            sizes.units,
                            sizes.summaries,
                            sizes.facts,
                            sizes.macros,
                            sizes.cooked_files,
                            sizes.cooked_declarations
                        );
                        // **The same table in bytes, which is what "5 GB" asks about.** The counts above say how
                        // many entries there are; `MemoryReport` says what each table costs, and the two differ by
                        // six orders of magnitude between a `#define` fact and a rendered closure. Written to the
                        // same `%TEMP%` file so a report of "it uses gigabytes" arrives with the answer attached.
                        let report = format!(
                            "renderings {} / {} KB | units {} | summaries {} / facts {} / macros {} | cooked {} / {}\n{}",
                            sizes.renderings,
                            sizes.rendering_bytes / 1024,
                            sizes.units,
                            sizes.summaries,
                            sizes.facts,
                            sizes.macros,
                            sizes.cooked_files,
                            sizes.cooked_declarations,
                            session.memory_report()
                        );
                        let _ = std::fs::write(
                            std::env::temp_dir().join("cpp_ls-memory.txt"),
                            report,
                        );

                        // **The pages are deliberately not handed back.** What was worth having is
                        // the *number* — `MemoryReport` in the file above — and the budgets that
                        // keep the live tables bounded.
                    }
                    if taken > 0 || prepared_count > 0 || !drain.is_empty() || !claimed.is_empty() {
                        // inside the slice that was. The numbers here are the split — prepared
                        // under the read lock, committed under the write one — so the next reader
                        // sees whether it was the parse, the insert or the queueing without
                        // re-instrumenting.
                        log::info!(
                            "index slice: prepared {prepared_count} in {prepared_in} ms, committed {taken}, write lock \
                             held {held} ms, {} to cook, {pending_now} left",
                            claimed.len()
                        );
                        log::info!(
                            "index cache: {reused} reused, {built} built, {unstored} unstored, {missing} missing"
                        );
                        if built > 0 {
                            let misses: Vec<String> = done
                                .iter()
                                .filter(|step| {
                                    step.outcome == cpp_code_analysis::StepOutcome::Built
                                })
                                .take(5)
                                .map(|step| step.path.display().to_string())
                                .collect();
                            log::info!("index cache misses: {misses:?}");
                        }
                    }
                    (pending_now, drain, claimed, held, taken)
                })
                .await
            else {
                log::warn!("the workspace was closed while it was being indexed");
                return;
            };

            // **The wave finds the size the machine has.** A wave that ran well over the budget halves, one that finished
            // well inside it grows by a file, and one that landed near it stays. `INDEX_SLICE` is the cap and
            // [`INDEX_SLICE_MS`] the target; what a fixed number costs is in [`THE_FIRST_WAVE`].
            // **Over the target halves at once**, not at twice it: measured, a rule that only halved above 80 ms let the
            // wave creep back to 32 while small headers were fast, and one heavy wave then held the writer for **1720
            // ms**. Under half the target grows by a file, so the search settles where the machine's files actually are.
            wave = match held {
                over if over > u128::from(INDEX_SLICE_MS) => (wave / 2).max(1),
                // **Doubling, not one file at a time.** The commit spends a budget of its own since 4.3, so the wave no
                // longer sets how long the writer is held — it sets how much is prepared (the read half) per slice.
                under if under < u128::from(INDEX_SLICE_MS) => (wave * 2).min(INDEX_SLICE),
                _ => wave,
            };

            // Gather cook inputs and prepare the drain under a read lock; render the owned cooks after releasing it.
            let drain_was_empty = drain.is_empty();
            let asked: Vec<std::path::PathBuf> = claimed.clone();
            let claimed_count = asked.len();
            log::trace!(
                "pump: drain starts (empty {}, {} claimed to cook)",
                drain_was_empty,
                asked.len()
            );
            let cooking_began = std::time::Instant::now();
            // **The stored-record decode, in holds of four.**
            //
            // This used to be one `run_blocking` for the whole slice — the decode of sixteen records, the gather of
            // the ones that have none, and the drain — and one lock hold paid for all of it: measured on neovim's
            // `main.c`, **1549 ms** with the writer's twenty-millisecond cook behind it and every request behind
            // that. The decode is why the slice is wide (each record is a whole closure's text), so the width stays
            // and the *hold* is cut: four records, then four more, then the drain.
            let mut stored_cooks = Vec::new();
            let mut cooks = Vec::new();
            for piece in asked.chunks(COOKS_A_HOLD) {
                let piece = piece.to_vec();
                let (records, gathered) = context
                    .analysis()
                    .run_blocking("read a cook slice", move |session| {
                        let mut records = Vec::new();
                        let mut cooks = Vec::new();
                        for path in &piece {
                            match session.a_stored_cook_record(path) {
                                Some(record) => records.push(record),
                                None => {
                                    if let Some(cook) = session.gather_a_cook(path) {
                                        cooks.push(cook);
                                    }
                                }
                            }
                        }
                        Some((records, cooks))
                    })
                    .await
                    .unwrap_or_default();
                // **The decode, off the lock.** A record is the file's whole rendering and decoding one is
                // 100–350 ms for a header's closure; doing it under the read guard put every request — and the
                // writer's twenty-millisecond cook — behind it (measured: **1549 ms** for one slice).
                let stored = tokio::task::block_in_place(|| {
                    records
                        .into_iter()
                        .filter_map(|(path, bytes)| {
                            match cpp_code_analysis::summary_codec::decode_the_cooked_record(&bytes)
                            {
                                Ok((cooked, Some(rendered))) => Some((path, cooked, rendered)),
                                _ => None,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                stored_cooks.extend(stored);
                cooks.extend(gathered);
            }
            let prepared = context
                .analysis()
                .run_blocking("prepare the drain", move |session| {
                    let revision = session.revision();
                    Some((
                        // **Bounded, because this is one read hold.** Two files to re-read and eight environments
                        // to render: the rest goes back on the queues and the next slice takes it.
                        session.prepare_the_drain(
                            drain,
                            DRAIN_FILES_A_HOLD,
                            DRAIN_ENVIRONMENTS_A_HOLD,
                        ),
                        revision,
                    ))
                })
                .await;
            let (prepared_drain, cooking_revision) = match prepared {
                Some((prepared, revision)) => (prepared, Some(revision)),
                None => (Default::default(), None),
            };
            let prepared_the_drain = cooking_began.elapsed().as_millis();
            let rendering = std::time::Instant::now();
            let rendered_cooks = context.analysis().render_cooks(cooks).await;
            let rendered_the_cooks = rendering.elapsed().as_millis();
            let slice = cpp_code_analysis::session::AClaimedSlice {
                stored: stored_cooks,
                fresh: rendered_cooks,
            };
            let built: Vec<std::path::PathBuf> =
                slice.paths().map(|path| path.to_path_buf()).collect();

            // **And the insert, under the write lock** — one `insert` per re-read summary and one `insert_cooked` per
            // cooked file, and nothing else.
            //
            // **In chunks, because that is what a claim size buys and what it must not cost.** The decode and the
            // render are off the lock and want a *large* claim (they parallelise over it); the insert is under the
            // lock and wants a *small* one (measured: claim 4 held the writer 931 ms, claim 32 held it **3165 ms**).
            // Splitting the slice into insert-sized pieces gives both — the claims are 16 wide, the writer is held for
            // four files at a time.
            log::trace!("pump: commit starts");
            let cook_logged = !asked.is_empty();
            if !asked.is_empty() {
                log::info!(
                    "cook slice: asked {}, rendered {}, emptied the queue, refused {}",
                    asked.len(),
                    built.len(),
                    asked.len().saturating_sub(built.len())
                );
            }
            let draining = std::time::Instant::now();
            let asked_for_the_write = asked.clone();
            let built_for_the_write = built.clone();
            let re_read = context
                .analysis()
                .update_session("index slice", move |session| {
                    // **The files this slice could not build are not asked for again by the closure walk**, which is
                    // what turned a cook that can never succeed into a loop: measured on a real session, the log
                    // repeated `4 to cook, 1 left` / `2 left` / `0 left` every second for as long as the window
                    // stayed open, at 28 % of a core. A query that names one of them clears its refusal
                    // (`Session::want_cooked_reading`), so nothing is lost but the retry.
                    if let Some(revision) = &cooking_revision {
                        if session.is_current(revision) {
                            session.refuse_the_cooks_that_built_nothing(
                                &asked_for_the_write,
                                &built_for_the_write,
                            );
                        } else if session.owns_revision(revision) {
                            for path in &asked_for_the_write {
                                session.want_cooked_reading(path);
                            }
                        }
                    }
                    session.commit_the_drain(prepared_drain)
                })
                .await
                .unwrap_or(0);
            let drained = draining.elapsed().as_millis();
            let inserting = std::time::Instant::now();
            let mut cooked = 0usize;
            let mut stored = slice.stored;
            let mut fresh = slice.fresh;
            while !stored.is_empty() || !fresh.is_empty() {
                let stored_here = stored.len().min(COMMIT_A_SLICE);
                let fresh_here = fresh.len().min(COMMIT_A_SLICE - stored_here);
                let chunk = cpp_code_analysis::session::AClaimedSlice {
                    stored: stored.drain(..stored_here).collect(),
                    fresh: fresh.drain(..fresh_here).collect(),
                };
                cooked += context
                    .analysis()
                    .update_session("index slice", move |session| {
                        session.commit_them(chunk).len()
                    })
                    .await
                    .unwrap_or(0);
            }
            let inserted = inserting.elapsed().as_millis();
            if cook_logged {
                log::info!(
                    "cook slice committed: {cooked} of {} rendering(s) stored, {re_read} re-read",
                    built.len()
                );
            }
            if re_read > 0 || cooked > 0 {
                log::debug!(
                    "index slice tail: re-read {re_read}, cooked {cooked}, write lock held {held} ms \
                     (re-read {drained} ms, cooked {inserted} ms)",
                    held = drained + inserted
                );
            }

            // **The inactive regions of the files this slice cooked**, sent as soon as the commit lands.
            //
            // A push has no request to be repeated, and this is the moment the answer becomes available: the spans
            // are a by-product of the cook, so before it a file has none (`None`) and after it the client is owed
            // them. The loop's idle branch is not enough — a slice ends with `continue`, so an iteration that cooked
            // never reaches it — and waiting for one that does would leave a file opened and left alone undimmed.
            if cooked > 0 {
                handlers::inactive_regions::publish_all(&context).await;
            }

            // **The pull's answer, computed while nobody is waiting.** A first pull for a new reading is the expensive
            // one (36.6 ms on `CodeFormat.cpp`, 109 ms on a cold session, against 2.2 ms for a repeat), and the pump is
            // the one party that knows when a new reading exists — it just committed one. `pending == 0` keeps it out of
            // a typing burst: the queue is empty, so no request is waiting on a parse this slice owes it.
            if !asked.is_empty() || re_read > 0 || cooked > 0 {
                log::info!(
                    "index tail: drain {prepared_the_drain} ms ({re_read} re-read), render {rendered_the_cooks} ms \
                     ({cooked} cooked), total {} ms",
                    cooking_began.elapsed().as_millis()
                );
            }

            if (re_read > 0 || cooked > 0) && pending == 0 {
                let warmed = context
                    .analysis()
                    .run_blocking("warm the pulls", |session| Some(session.warm_the_pulls()))
                    .await
                    .unwrap_or(0);
                if warmed > 0 {
                    log::trace!("pump: {warmed} pull answer(s) warmed");
                }
            }

            // **A slice that changed nothing must not be repeated for ever.** A file the cook claims and cannot build
            // — no summary, no unit, a rendering that declines — leaves `pending` where it was, and a pump that keeps
            // taking it never goes idle. Measured once on a real project: `index slice: … 2 to cook, 0 left` once a
            // second for as long as a window stayed open, and every diagnostic pull answered **1.4 s** late because
            // the pump held the write lock it was waiting for.
            //
            // Eight consecutive slices is not a heuristic about speed: an ordinary slice that finds the queue full of
            // files it must *re-read* always reports `prepared > 0`, and one that only cooks always reports `cooked`.
            // Eight in a row with nothing at all is a state, not a phase — so the files that cannot be built are
            // dropped from the queue (the next request that names one puts it back) and the pump is allowed to idle.
            //
            // **Only a slice that had work to do counts.** A wake with nothing pending is what a `prepare` for a file
            // that is already prepared produces, and counting those as "no progress" is how a single stuck file's
            // signal got diluted into a warning every eight seconds on an idle server.
            let after = context.analysis().with_snapshot(|session| {
                (
                    session.pending_work(),
                    before
                        .as_ref()
                        .is_some_and(|(_, revision)| !session.is_current(revision)),
                )
            });
            let pending = after.as_ref().map_or(pending, |(pending, _)| *pending);
            let had_work = before.as_ref().is_some_and(|(pending, _)| *pending > 0)
                || claimed_count > 0
                || !drain_was_empty;
            let progressed = committed > 0
                || re_read > 0
                || cooked > 0
                || after.as_ref().is_some_and(|(_, changed)| *changed)
                || before
                    .as_ref()
                    .is_some_and(|(previous, _)| pending < *previous);
            no_progress = if progressed || !had_work {
                0
            } else {
                no_progress + 1
            };
            if no_progress >= 8 {
                let dropped = context
                    .analysis()
                    .update_session("drop the files that cannot be built", |session| {
                        session.abandon_the_cooking_queue()
                    })
                    .await
                    .unwrap_or(0);
                log::warn!(
                    "the pump made no progress in {no_progress} slices with {pending} file(s) left: \
                     {dropped} queued file(s) were dropped (they are asked for again by the query that names one)"
                );
                no_progress = 0;
                if dropped == 0 {
                    context.analysis().wait_for_work(IDLE_WAIT).await;
                }
            }

            if pending > 0 {
                // **The bar counts the project's files, not the repair of their readings.** `pending` is
                // `pending_work()`, which since `8e976e1` includes the second pass — so `Load workspace` stayed on
                // screen for that pass too. Measured from a real session's own log: 565 slices, `0 to cook` on almost
                // every one, and 54 s before the bar retired, of which ~17 s is the first pass and the rest is the
                // re-read (947 timelines walked) plus decoding every summary a second time.
                //
                // A progress task is a promise about *the reader's project*, and the promise is kept when the index
                // knows which files exist and can answer questions about them; the second pass makes those answers
                // better, not available. So the bar follows the queue (`Session::pending`) and the pump keeps
                // draining everything (`was_pending` still reads `pending_work`).
                let (to_read, to_cook, to_reread, to_read_as_a_program) = match context
                    .analysis()
                    .query_blocking(|session| {
                        Some((
                            session.pending(),
                            session.pending_cooking(),
                            session.re_read_backlog(),
                            session.pending_units(),
                        ))
                    })
                    .await
                {
                    context::RequestOutcome::Ready(counts) => counts,
                    _ => (pending, 0, 0, 0),
                };
                let message = if to_read_as_a_program > 0 {
                    format!("{to_read} files to read, {to_read_as_a_program} program(s) to read")
                } else if to_read > 0 {
                    format!("{to_read} files to read, {to_cook} to cook, {to_reread} to re-read")
                } else {
                    format!(
                        "re-reading {to_reread} file(s), cooking {to_cook} file(s) for the compiler's view"
                    )
                };

                // **A message per slice is a message per keystroke's worth of work, and that is too many.** Slices are
                // as small as one file, so a cold start of this project sent **418 `$/progress` reports in seven
                // seconds** — a client that batches UI updates garbage-collects them into a dozen, and a client whose
                // pipe backs up blocks the server's writer in between (measured: a test client that only read while
                // waiting for its own answers filled the 64 KB pipe and parked the pump on a *write*, with no CPU and
                // a bar that never moved). A reader cannot perceive more than a few updates a second either, and the
                // count changes on almost every slice, so a rule that reports "when the sentence changes" reports
                // every time — the limit is the clock and nothing else. The final sentence is not lost: the last
                // slice before `pending == 0` is still reported, and the bar is ended there anyway.
                let now = std::time::Instant::now();
                if now.duration_since(last_report) >= PROGRESS_REPORT_EVERY {
                    context.status_bar().update_progress_task(
                        ProgressTask::LoadWorkspace,
                        None,
                        Some(message),
                    );
                    last_report = now;
                }

                // Yield between slices: the runtime is also serving requests, and a loop that never awaits would
                // hold the worker it runs on.
                was_pending = true;
                // **A round that did nothing waits; one that worked yields.** `yield_now` is what keeps
                // requests served while a slice is being built, and it is a *busy* loop when there is no slice:
                // measured on `test_cpp/src/containers.cpp` with cooking disabled, the pump took private bytes
                // from 476 MB to 1 228 MB in eight seconds while every stage it measures allocated **0 MB** —
                // the allocations were the loop itself, thousands of rounds a second for a backlog it could not
                // move. Waiting is the honest answer for a round with nothing to do; the notify that arrives
                // with real work wakes it.
                let did_nothing = prepared_count == 0
                    && claimed_count == 0
                    && drain_was_empty
                    && committed == 0
                    && re_read == 0
                    && cooked == 0;
                if did_nothing {
                    context.analysis().wait_for_work(IDLE_WAIT).await;
                } else {
                    tokio::task::yield_now().await;
                }
                continue;
            }

            // **`import std;` — the interface unit nothing in the project includes.** libstdc++ ships it as
            // `<c++ include>/bits/std.cc`, MSVC as `<VC>/Tools/MSVC/<version>/modules/std.ixx`; unless it is read,
            // every name the module exports is unknown, and the diagnostic that says so is right about the index and
            // wrong about the project.
            //
            // It happens **here**, in the pump, for two reasons that are both about who is allowed to write:
            //
            //   * a diagnostic is a read. It may be asked from several threads at once and must not add files to
            //     the index, take the analysis for writing, or cook a translation unit on the way to an answer;
            //   * the read is the whole STL closure (6 151 ms measured for `std` on a cold machine), and no request
            //     may hold the write lock for that. Queued as ordinary work, it is read over the same slices as
            //     every other file, and the request handlers keep the option of paying for it themselves when a
            //     reader is already waiting (`handlers::read_the_modules`).
            //
            // Only when the queue has drained: while it is filling, the modules are not what the answer is waiting
            // for, and queueing them would crowd out the project's own files.
            if first_pass || was_pending {
                let queued = context
                    .analysis()
                    .update_session("queue the modules the open files import", |session| {
                        session.queue_the_modules_the_open_files_import()
                    })
                    .await
                    .unwrap_or(0);

                if queued > 0 {
                    log::info!("{queued} module interface unit(s) queued for reading");
                    // Go round again rather than publish an answer computed before they were read: the next pass
                    // reads them in slices, and the end-of-drain refresh below runs once they are in.
                    was_pending = true;
                    tokio::task::yield_now().await;
                    continue;
                }
            }

            // Nothing to read. The first time that happens the project is loaded, and the diagnostics that were
            // waiting for it are published — after that the loop waits for the next edit.
            if first_pass {
                first_pass = false;
                settling_the_load = true;
                // **The bar ends after the post-drain work, not before it.** The queue being empty means every file
                // has been read; it does not mean the pump is done: the sharing cache is saved, the first inactive
                // regions are published, and the open file's program may still be read. Those are seconds of CPU on
                // a real project, and ending the indicator first made them look like runaway background work.

                // After the drain, so it holds everything this run learned; the entries carry their
                // own identity, so a stale file is inert rather than wrong.
                context
                    .analysis()
                    .update_session("save the sharing cache", |session| {
                        match session.save_the_sharing() {
                            Ok(written) => {
                                log::info!(
                                    "wrote {written} shared contribution(s) for the next run"
                                )
                            }
                            Err(error) => log::warn!("the sharing cache was not written: {error}"),
                        }
                    })
                    .await;

                // **The first load's inactive regions**, sent beside the diagnostics it just published: the client
                // opened its files before the project was read, so nothing had a rendering to answer from at
                // `didOpen` — and unlike a request, a notification is never asked for again.
                handlers::inactive_regions::publish_all(&context).await;

                // **The lenses, once — not on every edit.** A code lens is a count of files read, so the count
                // published before this file's closure was ready can be low; one refresh here asks for the settled
                // number. It is deliberately not repeated after body edits: `workspace/codeLens/refresh` asks the
                // client to replace its list, and a client is entitled to clear the old virtual lines before asking
                // again, which is the flicker this removes. The client already asks for its lenses when its text
                // changes; the handler serves its last list while a summary is rebuilt.
                if context.lsp_features().supports_refresh_code_lens() {
                    context.client().refresh_code_lenses();
                }
            } else if was_pending {
                // **…and the hints, for the same reason and with a measurement of their own.**
                //
                // A hint's parameter names come from the callee's declaration, so a request that arrives while the
                // index is filling is answered with fewer hints — a callee nobody has read yet resolves to nothing,
                // and a hint naming the wrong parameter would be worse than none. The answer therefore changes
                // without the document changing, and inlay hints carry no "incomplete" flag: a client that asked
                // once has no reason to ask again.
                //
                // Measured on the report that raised this: an edit re-queues the file for **~48 ms**, the editor asks
                // for hints immediately afterwards, and a person typing is inside that window at every keystroke —
                // so the hints vanished exactly while the code was being written. The handler used to refuse during
                // that window, which made it permanent; it now answers from what is known, and this is what makes
                // the incomplete answer whole.
                if context.lsp_features().supports_refresh_inlay_hint() {
                    context.client().refresh_inlay_hints();
                }

                // **…and the tokens, which have the same shape of answer and a longer memory.** A name is coloured
                // from its declaration, so a request that arrives before the index has read the header that declares
                // it draws that name with the client's own grammar colour — and a client caches the whole file's
                // tokens until the next edit, so nothing asks again. Measured from an editor: at startup `std::endl`
                // was drawn as a variable and became a function after any edit at all, which is that cache and that
                // silence. Nothing else in this loop notices: the document did not change.
                if context.lsp_features().supports_refresh_semantic_tokens() {
                    context.client().refresh_semantic_tokens();
                }
                // **…and the inactive regions, which are a *push* and therefore have no refresh to fall back on.**
                // A file's dead branches become answerable only when its cook lands (the per-file cook of the file's
                // own closure), which is after `didOpen` and after every `didChange`; nothing asks a server to repeat
                // a notification, so this is where the second answer goes out.
                handlers::inactive_regions::publish_all(&context).await;
            }
            let revision = context
                .analysis()
                .with_snapshot(|session| {
                    if last_diagnostic_revision
                        .as_ref()
                        .is_some_and(|previous| session.is_current(previous))
                    {
                        None
                    } else {
                        Some(session.revision())
                    }
                })
                .flatten();
            if let Some(revision) = revision {
                context.diagnostics().refresh_pull(context.clone());
                context
                    .diagnostics()
                    .schedule_open_files(context.clone())
                    .await;
                last_diagnostic_revision = Some(revision);
            }
            was_pending = false;

            // **A whole program the session owes, and the only thing that can wake it up for it.**
            //
            // The session defers reading a program as one piece — a walk, a cook, a parse of the whole program and a
            // sweep, measured at 5.09 s for 159 files — until nothing has been typed for a moment: a program read is
            // the session's one indivisible piece of work, and nothing may start one while a reader is typing. That
            // gate is a *clock*, and a clock wakes nothing: a reader who has stopped typing sends no notification, so
            // the one moment the read is safe is the one the pump would sleep through. This is the timer that makes
            // the gate a delay rather than a deadlock, and it sleeps the remainder rather than polling: a completed
            // program is taken off the schedule (`units_read`), so this fires at most once per open file.
            //

            if pending == 0
                && let Some(wait) = context
                    .analysis()
                    .with_snapshot(
                        cpp_code_analysis::Session::the_wait_before_a_program_may_be_read,
                    )
                    .flatten()
            {
                if wait.is_zero() {
                    // **The program read is the last CPU-heavy part of a first load.** Keep the bar on screen for
                    // it: on the neovim checkout this is seconds of parsing and a reader has no other signal that it
                    // is still loading.
                    if settling_the_load {
                        context.status_bar().update_progress_task(
                            ProgressTask::LoadWorkspace,
                            None,
                            Some("reading the program for the compiler's view".to_string()),
                        );
                    }
                    // **Then breathe.** A project has one program per translation unit, and a pump that read them
                    // back to back would peg a core for minutes after every pause; one idle tick between them is
                    // the same pace the loop uses when it has nothing to do.
                    //
                    // **And a program the session *declines* breathes too.** `read_a_program` answers `false` both
                    // when nothing is owed and when the file it chose is over the byte budget — and an over-budget
                    // file is *deliberately* re-considered on every drain (it may fit after an edit or a budget
                    // frees). This branch used to `continue` with no wait in the second case, so the pump ran the
                    // whole decision again at once: `the_program_to_read` walks the closure of **every open file** to
                    // price it, so a project with twenty tabs open spun on twenty closure walks — measured on
                    // `test_cpp` with no request at all: 1 094 MB in 34 s, killed by a 1 GB cap, one core busy, and
                    // every pump stage reporting 0 MB. `CPPLS_NO_PROGRAM_READ=1` made the same run flat at 511 MB
                    // for 60 s, which is how the read was named.
                    let read = context.analysis().read_a_program().await;
                    if read {
                        context.diagnostics().refresh_pull(context.clone());
                        context
                            .diagnostics()
                            .schedule_open_files(context.clone())
                            .await;
                        if context.lsp_features().supports_refresh_inlay_hint() {
                            context.client().refresh_inlay_hints();
                        }
                        if context.lsp_features().supports_refresh_semantic_tokens() {
                            context.client().refresh_semantic_tokens();
                        }
                    }
                    // **The bar retires when no program is still owed**, not when the first one finished: a
                    // project has one program per open translation unit.
                    if settling_the_load {
                        let still_owed = read
                            && context
                                .analysis()
                                .with_snapshot(|session| {
                                    session.the_wait_before_a_program_may_be_read().is_some()
                                })
                                .unwrap_or(false);
                        if !still_owed {
                            finish_the_load(&context, &load);
                            settling_the_load = false;
                        }
                    }
                    context.analysis().wait_for_work(IDLE_WAIT).await;
                } else {
                    // A deferred program is work for a later idle moment, not work the current load indicator
                    // should wait for; the reader has stopped typing and the bar can retire.
                    if settling_the_load {
                        finish_the_load(&context, &load);
                        settling_the_load = false;
                    }
                    context.analysis().wait_for_work(wait).await;
                }
                continue;
            }

            // **The load settles when no program is owed and nothing is pending.** A deferred program (wait > 0)
            // has its own idle moment; the next edit will schedule it and no progress bar can wait for typing.
            if settling_the_load {
                finish_the_load(&context, &load);
                settling_the_load = false;
            }

            // **A timeout is not work.** `wait_for_work` answers whether something was queued while the loop waited;
            // going round on `false` ran a *full slice* — a parse wave, a commit, a cook claim, all empty — once a
            // second for as long as the window stayed open. That is the `index slice: … 0 to cook, 0 left` line
            // repeated for ever in a reader's log, plus a no-progress warning every eighth slice, and it holds the
            // analysis for a moment each time. Nothing is pending when the loop reaches here (the `pending > 0` branch
            // above continues first), so a timeout only means "wait again".
            if !context.analysis().wait_for_work(IDLE_WAIT).await {
                continue;
            }
        }
    });
}

/// End the load indicator only once the pump has nothing left that it started — see `settling_the_load`.
///
/// **The guard is what makes this safe to miss.** The pump has several exits, and two of them used to leave the
/// reader's bar frozen at its last report (a session superseded while it was being built, a pump that stopped
/// because the workspace was reloaded): neither called this. `LoadIndicator` ends the bar when it is dropped, and
/// this retires it early for the ordinary "the load is settled, the rest is idle work" case.
fn finish_the_load(context: &ServerContextSnapshot, load: &crate::context::LoadIndicator) {
    load.retire();
    log::info!(
        "the workspace is indexed — the longest an update held the analysis was {} ms",
        context.analysis().worst_write_hold()
    );
}

/// The folders the client opened.
///
/// `workspaceFolders` first, then the deprecated `rootUri` — which most clients still send, and which is the only
/// thing a client that predates workspace folders has.
pub fn workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(folders) = &params.workspace_folders {
        for folder in folders {
            if let Some(path) = uri_to_file_path(&folder.uri) {
                roots.push(path);
            }
        }
    }

    if roots.is_empty() {
        #[allow(deprecated)]
        if let Some(path) = params.root_uri.as_ref().and_then(uri_to_file_path) {
            roots.push(path);
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cpp_code_analysis::{CompilerConfig, Session, WatchFilter};
    use lsp_server::Connection;
    use lsp_types::{ClientCapabilities, TextDocumentContentChangeEvent};

    use super::*;
    use crate::context::{ClientId, ServerContext};
    use crate::util::path_to_uri;

    fn prepared(
        context: &ServerContextSnapshot,
        root: &std::path::Path,
    ) -> Session<cpp_code_analysis::DiskFiles> {
        Session::with_config(
            root,
            context.analysis().files().clone(),
            WatchFilter::new(root),
            CompilerConfig::default(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_built_session_receives_the_latest_buffer_at_installation() {
        let (connection, _client) = Connection::memory();
        let server = ServerContext::new(
            Arc::new(connection),
            ClientCapabilities::default(),
            ClientId::Other,
            None,
        );
        let context = server.snapshot();
        let root = std::env::temp_dir().join("cpp-ls-install-session");
        let path = root.join("main.cpp");
        let uri = path_to_uri(&path).unwrap();
        let session = prepared(&context, &root);
        let (version, generation) = {
            let mut manager = context.workspace_manager().lock().await;
            manager.set_roots(vec![root]);
            manager.open_file(uri.clone(), "int old_name;".into(), 1);
            manager
                .change_file(
                    &uri,
                    2,
                    &[TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: "int current_name;".into(),
                    }],
                )
                .unwrap();
            (manager.workspace_version(), manager.reload_generation())
        };
        assert!(install_prepared_session(&context, session, version, generation).await);
        let text = context
            .analysis()
            .with_snapshot(|session| session.files().held(&path).unwrap().text.clone())
            .unwrap();
        assert_eq!(text.as_ref(), "int current_name;");
        server.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_built_for_an_old_workspace_is_not_installed() {
        let (connection, _client) = Connection::memory();
        let server = ServerContext::new(
            Arc::new(connection),
            ClientCapabilities::default(),
            ClientId::Other,
            None,
        );
        let context = server.snapshot();
        let root = std::env::temp_dir().join("cpp-ls-stale-session");
        let session = prepared(&context, &root);
        let (version, generation) = {
            let mut manager = context.workspace_manager().lock().await;
            manager.set_roots(vec![root]);
            let captured = (manager.workspace_version(), manager.reload_generation());
            manager.clear_workspace().await;
            captured
        };
        assert!(!install_prepared_session(&context, session, version, generation).await);
        assert!(context.analysis().with_snapshot(|_| ()).is_none());
        server.close().await;
    }
}
