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
        let open = {
            let workspace_manager = context.workspace_manager().lock().await;
            workspace_manager.is_open_file(&change.uri)
        };

        match change.typ {
            FileChangeType::DELETED => {
                deleted.push(change.uri.clone());
                if !open {
                    changes.push(cpp_code_analysis::FileEvent::removed(path.clone()));
                }
            }
            FileChangeType::CREATED | FileChangeType::CHANGED => {
                if open {
                    continue;
                }

                changes.push(cpp_code_analysis::FileEvent::modified(path.clone()));
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

        // **The answer changed and the document did not** — a watched file is not one the client is editing, so
        // nothing about the text it is showing has moved. It used to be told (`workspace/diagnostic/refresh`).
        //
        // **It is not told any more, and that is the decision rather than an omission.** A pull client asks on its own
        // rhythm, and the request made it ask inside a loop — see the note in `handlers/initialized` — so what a
        // watched-file event does is queue the work and wake the pump, nothing else.
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
