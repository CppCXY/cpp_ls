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
//! 7. file watches     the globs the client should tell us about, registered dynamically
//! 8. diagnostics      the first full pass, once the index holds something
//! ```
//!
//! Steps 6 to 8 are what make this a *language server* rather than a query engine: the session is usable the moment
//! step 4 returns, and everything after it makes the answers better without making the editor wait.

mod client_config;

use std::path::PathBuf;

use lsp_types::InitializeParams;

use crate::cmd_args::CmdArgs;
use crate::context::{ProgressTask, ServerContextSnapshot, get_client_id};
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

/// How many files one background slice reads before it lets the runtime run something else.
///
/// A slice is a transaction against the analysis: it takes the write lock once and gives it back, so a query
/// arriving mid-index waits for a slice and not for a project. A slice's files are parsed on all the cores at once
/// (`Session::advance`), so thirty-two files hold the lock for about what sixteen did when they were parsed one at a
/// time — short enough that a keystroke does not feel it, long enough that the indexing does not spend its time
/// taking locks.
///
/// # What was tried here, and why it is still a count
///
/// A **time budget** instead of a count: advance until twenty-five milliseconds have gone, on the theory that a
/// count cannot be right for both small files and four-megabyte headers. Measured, it was **worse** — the answers
/// stayed empty for the first nine completions rather than the first seven, and completions that were fast were
/// fast because they had *nothing to say*. A slice that ends early drains the queue later, and a list with no
/// summary behind it is not a fast answer but no answer. Throughput and latency pull against each other here, and
/// this count already sat at a workable point between them.
const INDEX_SLICE: usize = 4;

/// **How many files one slice cooks** — the same shape as [`INDEX_SLICE`], and the same reason.
///
/// Cooking a file is a render and a parse of it, which is the largest single thing this pump does: measured
/// `own 9–269 ms` per file against the `unit 15–42 ms` a lookup costs. The count is small because the work is
/// large, and because the expensive half now runs under a **read** lock ([`Session::render_them`]) while only the
/// insert needs the write one.
const COOK_A_SLICE: usize = 4;

/// **How long one slice may hold the analysis write lock** — the number the count above is a proxy for.
///
/// Measured, thirty-two files held the lock for anywhere between **84 and 2430 ms**, and a slice of *zero* files —
/// the cooking half of `Session::advance` — for up to **1527 ms**. This is what a completion arriving inside that
/// window waits for.
///
/// Bounding the slice *by* this number was tried and is worse: slices of 76–356 ms, and the completion list came
/// back **empty for the first eight requests** instead of the first five. The two knobs trade a wait for an empty
/// list, and no setting of either wins, because reading files and answering questions take the same write lock.
/// The constant is kept as the record of what the wait actually is.
#[allow(dead_code)]
const INDEX_SLICE_MS: u64 = 40;

pub async fn initialized_handler(
    context: ServerContextSnapshot,
    params: InitializeParams,
    cmd_args: CmdArgs,
) -> Option<()> {
    let roots = workspace_roots(&params);
    init_logger(roots.first().and_then(|root| root.to_str()), &cmd_args);

    let client_id = match &cmd_args.editor {
        Some(editor) => editor.clone().into(),
        None => get_client_id(&params.client_info),
    };
    let supports_config_request = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.configuration)
        .unwrap_or_default();
    log::info!("client {client_id:?}, workspace roots {roots:?}");

    // The client's configuration is read **before** the session is opened, because one of the things it says —
    // which files are not the project's — is part of the filter the session is opened with.
    let client_config = get_client_config(&context, client_id, supports_config_request).await;
    log::info!("client config: {client_config:?}");
    {
        let mut workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.set_roots(roots.clone());
        workspace_manager.set_client_config(client_config);
        // The `--config` a caller named is kept, because a reload re-opens the session and has no `CmdArgs` of its
        // own: the file the user pointed at is part of what the workspace *is*.
        workspace_manager.set_config_file(cmd_args.config.0.clone().map(PathBuf::from));
    }

    start_analysis(context.clone()).await;
    register_files_watch(context.clone()).await;

    Some(())
}

/// Open the analysis over the workspace and start reading it.
///
/// Called once at `initialized` and again on every workspace reload (`WorkspaceManager`), which is why it takes the
/// context and asks the state where the workspace is rather than being handed a root.
pub async fn start_analysis(context: ServerContextSnapshot) {
    let Some(root) = open_session(&context).await else {
        return;
    };

    context.status_bar().update_progress_task(
        ProgressTask::LoadWorkspace,
        None,
        Some(format!("Indexing {}", root.display())),
    );

    index_in_background(context).await;
}

/// Open a session over the workspace root, with the client's buffers already in front of it.
///
/// `None` when the client has named no folder: there is nothing to open over, and every query answers "no analysis"
/// until it does — which is the honest state rather than an empty project.
async fn open_session(context: &ServerContextSnapshot) -> Option<PathBuf> {
    let (root, filter, config_file) = {
        let workspace_manager = context.workspace_manager().lock().await;
        (
            workspace_manager.root()?.to_path_buf(),
            workspace_manager.watch_filter()?,
            workspace_manager.config_file().map(PathBuf::from),
        )
    };

    context
        .status_bar()
        .create_progress_task(ProgressTask::LoadWorkspace)
        .await;

    // The compiler is run here (`Session::open` asks it for its search paths), so this is the one slow call in the
    // handshake, and it happens on the blocking pool rather than on the runtime's own thread.
    context
        .analysis()
        .open(root.clone(), filter, config_file)
        .await;

    report_project_configuration(context);

    // The open buffers are read **after** the session exists, and the order is the point: a client that restarted
    // with unsaved files would otherwise have the analysis answer about the disk, and a change that arrived while
    // the session was being built (the update queue is a separate task) is in this list because it is read now.
    let open_files = {
        let workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.workspace_open_files()
    };

    // **The sharing cache, warm** — §8 step 4, and before the first file is read, so the units this run builds can
    // splice last run's contributions instead of re-walking them. A missing file is a cold start, not an error.
    context
        .analysis()
        .update_session("initialized: the sharing cache", |session| {
            let restored = session.load_the_sharing();
            if restored > 0 {
                log::info!("restored {restored} shared contribution(s) from the last run");
            }
        })
        .await;
    if !open_files.is_empty() {
        let count = open_files.len();
        context
            .analysis()
            .update_session(
                "initialized: the files the client had open",
                move |session| {
                    for (uri, text) in &open_files {
                        if let Some(path) = uri_to_file_path(uri) {
                            session.did_open(path, text);
                        }
                    }
                },
            )
            .await;
        log::info!("re-marked {count} open buffers as the text the analysis reads");
    }

    log::info!("analysing {}", root.display());
    Some(root)
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
async fn index_in_background(context: ServerContextSnapshot) {
    tokio::spawn(async move {
        let version = context.workspace_manager().lock().await.workspace_version();
        let mut first_pass = true;
        // What the last slice left to do — see the drain below, which is the moment a file's **cooked** reading
        // changes hands.
        let mut was_pending = false;
        // **How many slices in a row changed nothing** — see the breaker after the commit. A slice that claims a file
        // and cannot build it leaves the queue's count where it was, and a loop that keeps taking it is a pump that
        // never goes idle: measured once as `index slice: … 2 to cook, 0 left` once a second for as long as a window
        // stayed open, with every diagnostic pull answered 1.4 s late because the pump held the lock it waited for.
        let mut no_progress = 0usize;

        // **The wave the pump is asking for**, adjusted after every slice (see below). It starts small because the first
        // wave of a cold start is the expensive one — measured: a fixed cap of 32 held the writer for 3309 ms on the
        // latency fixture's first wave and a completion that arrived inside it waited for all of it; a fixed 4 answered in
        // 137 ms. That is the whole reason this is a measurement rather than a constant.
        const THE_FIRST_WAVE: usize = 8;
        let mut wave = THE_FIRST_WAVE;

        loop {
            // **The pump's own heartbeat, at `trace`.** A bar stuck at "re-reading N" with no CPU means the loop is
            // parked on an `await`, and which one is invisible from the outside: every step of a slice is silent when
            // the slice finds nothing to do. These five lines are the only way to tell "blocked on the workspace
            // manager" from "blocked on the analysis" from "blocked in the drain", and they cost nothing when the
            // level is not `trace`.
            log::trace!("pump: round");
            // A reload bumps the version and starts a pump of its own: this one stops rather than racing it for
            // the write lock, because its idea of what the project is is the one that was replaced.
            if context.workspace_manager().lock().await.workspace_version() != version {
                log::debug!("the workspace was reloaded; this indexing pass stops");
                return;
            }

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
            let prepared = context
                .analysis()
                .run_blocking(move |session| Some(session.prepare_a_wave(wave)))
                .await
                .unwrap_or_default();
            let prepared_count = prepared.prepared.len();
            log::trace!("pump: wave prepared ({prepared_count})");

            let Some((pending, drain, claimed, held)) = context
                .analysis()
                .update_session("index slice", |session| {
                    // **Timed, and split, because the count is only a proxy for the one number a caller can feel:**
                    // how long the write lock is held. The parse is no longer in this window — it was paid for under
                    // the read lock above — and neither is the drain that follows the inserts, which comes back as a
                    // plan and is paid for under the read lock below. What is left is the insert and the queueing.
                    let started = std::time::Instant::now();
                    let (done, drain) = session.commit_a_wave(prepared, wave);
                    let taken = done.len();
                    let held = started.elapsed().as_millis();

                    // **All the work, not just the indexing queue.** A session that has read every file still has
                    // the files its open ones include to read *as a compiler reads them* (`Session::cook`), and this
                    // loop is what gives that work a caller: reading the queue alone would stop the pump one step
                    // before the declarations a reader asks about exist.
                    //
                    // **And which files the next cooking slice is for**, which is a queue pop and nothing more: what
                    // a cook needs is gathered, rendered and parsed under the read lock below, beside whatever a
                    // person is typing ([`Session::materials_for`], [`Session::render_them`]).
                    let claimed = session.claim_cooking(COOK_A_SLICE);

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
                            "index drained: cache {} rendering(s) ({} KB), {} translation unit(s), {} summar(ies), \\
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

                        // **The pages are deliberately not handed back.** The measurement says the resident set
                        // is ~515 MB of free pages the allocator holds (`docs/memory-audit.md` §M1 之后), and the
                        // decision is to keep them: the next index burst, the next cook and the next keystroke ask
                        // for exactly that memory, and a working-set trim would only make them fault it back in.
                        // What was worth having is the *number* — `MemoryReport` in the file above — and the
                        // budgets that keep the live tables bounded.
                    }
                    if taken > 0 || prepared_count > 0 || !drain.is_empty() || !claimed.is_empty() {
                        // inside the slice that was. The numbers here are the split — prepared under the read lock,
                        // committed under the write one — so the next reader sees whether it was the parse, the
                        // insert or the queueing without re-instrumenting. `docs/cold-start-cpu.md` asks for exactly
                        // this and the request was recorded there rather than in a plan.
                        log::info!(
                            "index slice: prepared {prepared_count}, committed {taken}, write lock held {held} ms, \
                             {cooking} to cook, {pending_now} left",
                            cooking = claimed.len()
                        );
                    }
                    (pending_now, drain, claimed, held)
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
                under if under < u128::from(INDEX_SLICE_MS) / 2 => (wave + 1).min(INDEX_SLICE),
                _ => wave,
            };

            // **The drain's and the cooking slice's expensive halves, under one read lock.** Measured, the second
            // pass is `150 ms` per file, a rendering is `9–269 ms` and gathering a cook's materials is `15–42 ms` —
            // and together they were the largest thing left inside the write lock, holding it for up to **572 ms** on
            // a slice that committed *no* files at all. None of them changes anything, so none of them belongs under
            // the lock a query waits for.
            // …and the same for the drain and the cooking slice: a rendering is 9–269 ms per file and a unit walk is
            // 150 ms, so this phase is the longest remaining uninterrupted span of CPU in the loop.
            //
            // **Taken before the closures consume them**, for the breaker below and for the refusals: what a slice
            // *asked for* is half of "did it achieve anything", and the other half comes back from the commit.
            let drain_was_empty = drain.is_empty();
            let asked: Vec<std::path::PathBuf> = claimed.clone();
            let claimed_count = asked.len();
            log::trace!(
                "pump: drain starts (empty {}, {} claimed to cook)",
                drain_was_empty,
                asked.len()
            );
            let (prepared_drain, rendered_cooks, built) = context
                .analysis()
                .run_blocking(move |session| {
                    let materials = session.materials_for(claimed);
                    let rendered = session.render_them(materials);
                    // **What actually came back, not what was asked for.** `materials_for` answering with a file is
                    // only half the question — the render is the other half, and it declines on its own grounds
                    // (`Session::render_them`). Measured from a reader's log: `4 to cook` every second with
                    // `committed 0`, because the four files gathered materials and then rendered nothing, so nothing
                    // was ever refused and the closure walk wanted them again on the next drain.
                    let built: Vec<std::path::PathBuf> = rendered
                        .iter()
                        .map(|(indexed, _)| indexed.summary.path.clone())
                        .collect();
                    Some((session.prepare_the_drain(drain), rendered, built))
                })
                .await
                .unwrap_or_default();

            // **And the insert, under the write lock** — one `insert` per re-read summary and one `insert_cooked` per
            // cooked file, and nothing else. Both halves are timed, because the count is not the number a caller
            // feels: what makes this product slow is a write lock held for long, and this line says which half did it.
            log::trace!("pump: commit starts");
            let (re_read, cooked) = context
                .analysis()
                .update_session("index slice", |session| {
                    // **The files this slice could not build are not asked for again by the closure walk**, which is
                    // what turned a cook that can never succeed into a loop: measured on a real session, the log
                    // repeated `4 to cook, 1 left` / `2 left` / `0 left` every second for as long as the window
                    // stayed open, at 28 % of a core. A query that names one of them clears its refusal
                    // (`Session::want_cooked_reading`), so nothing is lost but the retry.
                    session.refuse_the_cooks_that_built_nothing(&asked, &built);
                    let cook_logged = !asked.is_empty();
                    if !asked.is_empty() {
                        log::info!(
                            "cook slice: asked {}, rendered {}, emptied the queue, refused {}",
                            asked.len(),
                            built.len(),
                            asked.len().saturating_sub(built.len())
                        );
                    }
                    let started = std::time::Instant::now();
                    let re_read = session.commit_the_drain(prepared_drain);
                    let drained = started.elapsed().as_millis();
                    let started = std::time::Instant::now();
                    let cooked = session.commit_them(rendered_cooks).len();
                    let inserted = started.elapsed().as_millis();
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
                    (re_read, cooked)
                })
                .await
                .unwrap_or((0, 0));

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
            let had_work = pending > 0 || claimed_count > 0 || !drain_was_empty;
            let progressed = prepared_count > 0
                || !drain_was_empty
                || claimed_count > 0
                || re_read > 0
                || cooked > 0;
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
                let to_read = match context
                    .analysis()
                    .query_blocking(|session| Some(session.pending()))
                    .await
                {
                    crate::context::RequestOutcome::Ready(count) => count,
                    _ => pending,
                };
                // **And the second pass, which the queue count does not see.** `pending` is the count of files the
                // session still has to *read*; the re-read pass that turns each of them into a compiler's reading is
                // `pending_work() - pending`, and a bar that only counts the first says "0 files to read" while the
                // machine is busy — reported from an editor as "the read finished and it is still working, so I
                // thought it had hung".
                let still = pending.saturating_sub(to_read);
                let message = if to_read > 0 {
                    format!("{to_read} files to read, {still} to re-read")
                } else {
                    format!("re-reading {still} file(s) for the compiler's view")
                };
                context.status_bar().update_progress_task(
                    ProgressTask::LoadWorkspace,
                    None,
                    Some(message),
                );

                // Yield between slices: the runtime is also serving requests, and a loop that never awaits would
                // hold the worker it runs on.
                was_pending = true;
                tokio::task::yield_now().await;
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
                // **Not finished here** — see the note above the program read: the bar's end is owed until nothing is
                // left to read as a program.
                // **The one number that bounds how long a request can have been made to wait.** A query and an update
                // take the same lock, so this is the worst case *by construction*: if a completion felt slow and this
                // says 12 ms, the wait was inside that completion's own work rather than in the queue behind the
                // indexer — and if it says 701, the queue is the whole story and no amount of query tuning helps.
                log::info!(
                    "the workspace is indexed — the longest an update held the analysis was {} ms",
                    context.analysis().worst_write_hold()
                );

                // **Written for the next run** — `docs/walk-redesign.md` §8 step 4. After the drain, so it holds
                // everything this run learned; the entries carry their own identity, so a stale file is inert rather
                // than wrong.
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
            } else if was_pending {
                // **The queue drained — and the server says nothing about it.**
                //
                // A file's cooked reading is built from the environment of a complete translation unit, so the
                // answers for an open file *do* change here: between the edit and this moment its diagnostics are read
                // off its own text, which is the coarser reading. The server used to announce it with
                // `workspace/diagnostic/refresh`.
                //
                // **That is gone on purpose, and it is not coming back.** A pull client already asks on its own
                // rhythm, and the server's request made it ask in a *loop*: a reader's log showed
                // `republishing open file diagnostics` alternating with an `index slice` cycle that asked for the same
                // nine cooks every second and committed nothing, at a full core, for as long as the window stayed
                // open. There is no workspace diagnostic *report* in this server either
                // (`workspace_diagnostics: false`), so the refresh named nothing to redo — it was a request to repeat
                // work that had just been done.
                //
                // What replaces it is the client's own next ask, answered from the index as it stands.

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
            // `read_a_program` is three phases — the walk under the read lock, the 4.9 s of cook/parse/sweep with
            // **no lock at all**, and the inserts under the writer — which is why a real project's program can be read
            // now and could not be before (`docs/architecture.md` §7.4).

            if pending == 0
                && let Some(wait) = context
                    .analysis()
                    .with_snapshot(
                        cpp_code_analysis::Session::the_wait_before_a_program_may_be_read,
                    )
                    .flatten()
            {
                if wait.is_zero() {
                    // **Then breathe.** A project has one program per translation unit, and a pump that read them
                    // back to back would peg a core for minutes after every pause; one idle tick between them is
                    // the same pace the loop uses when it has nothing to do.
                    if context.analysis().read_a_program().await {
                        tokio::time::sleep(IDLE_WAIT).await;
                    }
                } else {
                    tokio::time::sleep(wait + IDLE_WAIT).await;
                }
                continue;
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
