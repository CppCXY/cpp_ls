//! # `workspace/didChangeWatchedFiles` — the client's own filesystem news
//!
//! This server does **not** watch the filesystem. LSP has a notification for exactly this, clients implement it,
//! and a client's watcher is better informed than any server's: it is the process that wrote the file, it knows
//! about an atomic save's temporary rename, and it does not have to poll. So the design is: the client watches and
//! tells us, and this module is what receives it.
//!
//! ```text
//! created / changed ──▶ session.changed([FileEvent]) ──▶ the index decides what to re-read ──▶ diagnostics
//! deleted           ──▶ the same, and the client's list for that file is cleared
//! ```
//!
//! The analysis decides the rest: a modified header is one file to re-read and not the fifty files that include it,
//! a created file may *outrank* the one an include resolved to, and a deleted file's includers have to be re-read
//! because their search would now fail. All of that is `SummaryStore::respond`, and the response is what this module
//! turns into work.

use lsp_types::{DidChangeWatchedFilesParams, FileChangeType};

use crate::context::{ServerContextSnapshot, UpdateEvent};
use crate::util::uri_to_file_path;

pub async fn on_did_change_watched_files(
    context: ServerContextSnapshot,
    params: DidChangeWatchedFilesParams,
) -> Option<()> {
    context
        .enqueue(UpdateEvent::WatchedFilesChanged(params))
        .await;
    Some(())
}

/// **The native watcher lost events, or the tree changed shape** — answer by asking the disk what moved.
///
/// This is the replacement for what used to happen (a full workspace reload) and the reason it exists: a lost-event
/// report is common on a large tree, and a reload is a compiler run, a whole-project index and a whole-program read.
/// Measured on a Linux kernel checkout, the log held **332 overflow reports and four reloads in fifty seconds** —
/// each reload re-read 361 files and committed them under the write lock for five seconds, which is exactly the
/// window in which the diagnostic pulls took 4.9 s each. [`Session::reconcile`] answers the same question with a
/// `stat` per project file and a walk.
///
/// The coalescing is the watcher's (`a reconcile is already in flight` in `native_watcher`): this handler runs once per
/// window whatever the storm's size.
///
/// # Why the disk is asked before the writer is taken
///
/// Because the stats are the expensive half — 400–850 ms for 3 000 files on Windows, the listing being most of it — and
/// a write hold that long is a request's latency (the very complaint the reload was removed for). So
/// [`Session::prepare_a_reconcile`] runs under the **read** lock (queries unaffected), the answer crosses to
/// [`Session::commit_a_reconcile`] under the writer, and the writer is held for the session's own bookkeeping only.
pub async fn process_reconcile(context: ServerContextSnapshot, listing: bool) -> Option<()> {
    let started = std::time::Instant::now();
    let Some(answer) = context
        .analysis()
        .with_snapshot(|session| session.prepare_a_reconcile(listing))
    else {
        // No workspace yet: a reconcile about a project nobody has opened is nothing.
        return Some(());
    };
    let asked_for = started.elapsed();

    if answer.is_empty() {
        // The ordinary answer, and the one worth saying out loud: the storm reported "events were lost" and the disk
        // agreed with the index. A rescan that finds nothing must not look like a rescan that ran.
        log::info!("reconciled with the disk in {asked_for:.1?}: nothing moved");
        return Some(());
    }

    let moved = (
        answer.modified.len(),
        answer.removed.len(),
        answer.new.len(),
    );
    let response = context
        .analysis()
        .update_session("apply a reconcile", move |session| {
            session.commit_a_reconcile(answer)
        })
        .await;

    let Some(response) = response else {
        return Some(());
    };

    log::info!(
        "reconciled with the disk in {asked_for:.1?} ({} changed, {} gone, {} new): {} file(s) to re-read, {} forgotten",
        moved.0,
        moved.1,
        moved.2,
        response.reindex.len(),
        response.forgotten.len()
    );
    context.analysis().wake();
    context
        .diagnostics()
        .schedule_open_files(context.clone())
        .await;
    Some(())
}

pub async fn process_did_change_watched_files(
    context: ServerContextSnapshot,
    params: DidChangeWatchedFilesParams,
) -> Option<()> {
    let mut changes = Vec::new();
    let mut deleted = Vec::new();
    let mut configuration_files = Vec::new();
    let mut reload_needed = false;

    for change in params.changes {
        let Some(path) = uri_to_file_path(&change.uri) else {
            continue;
        };

        // A buffer the client has open is the text, and the disk is not: a save reports both a `didSave` and a
        // watched-file change, and re-reading the file would answer about text the user cannot see.
        //
        // **Except a configuration file, which is not a text to index but a reason to reload.** It has no buffer
        // reading in the analysis at all — the project's configuration is read from disk when a session opens — and
        // `didSave` carries no text (the server asks for none), so treating an open `.cppls.toml` like an open source
        // file left a save with **no effect at all** until the file was closed or the server restarted
        // (`docs/language-server-review.md` P1-4).
        let (open, configuration) = {
            let workspace_manager = context.workspace_manager().lock().await;
            (
                workspace_manager.is_open_file(&change.uri),
                workspace_manager.is_configuration_file(&path),
            )
        };

        match change.typ {
            FileChangeType::DELETED => {
                deleted.push(change.uri.clone());
                if !open {
                    changes.push(cpp_code_analysis::FileEvent::removed(path.clone()));
                }
            }
            FileChangeType::CREATED | FileChangeType::CHANGED => {
                if open && !configuration {
                    continue;
                }
                // **And no source event for a configuration file**, open or not: a `.toml` is not a translation unit,
                // and what a change to it means is the reload at the bottom of this loop.
                if !configuration {
                    changes.push(cpp_code_analysis::FileEvent::modified(path.clone()));
                }
            }
            _ => continue,
        }

        // The project's build description is not indexed — it *configures* the index, so a change to it is a
        // reload rather than an edit. `WorkspaceManager` decides, and debounces.
        let workspace_manager = context.workspace_manager().lock().await;
        if workspace_manager.is_configuration_file(&path) {
            configuration_files.push(path);
            reload_needed = true;
        }
    }

    if !changes.is_empty() {
        let response = context
            .analysis()
            .update_session("a watched file changed", move |session| {
                session.changed(changes)
            })
            .await;

        let Some(response) = response else {
            // No workspace yet: the events are about a project nobody has opened.
            return Some(());
        };

        // The events queued work — re-reading the files that include what changed, or the whole project after a
        // configuration change — and the pump is what reads it.
        context.analysis().wake();
        context
            .diagnostics()
            .schedule_open_files(context.clone())
            .await;
        if response.everything {
            log::info!("the whole project has to be read again");
        }
    }

    // **Nothing to clear.** A deleted file is one a pull client asks about and is answered with an empty report; the
    // list it holds is its own, and no message from here can go stale in it.
    let _ = deleted;

    if reload_needed {
        for path in configuration_files {
            context
                .workspace_manager()
                .lock()
                .await
                .add_update_config_task(context.clone(), path)
                .await;
        }
    }

    Some(())
}
