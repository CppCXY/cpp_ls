//! # `workspace/didChangeWatchedFiles` — the client's own filesystem news
//!
//! This server does **not** watch the filesystem. LSP has a notification for exactly this, clients implement it,
//! and a client's watcher is better informed than any server's: it is the process that wrote the file, it knows
//! about an atomic save's temporary rename, and it does not have to poll. So the design is: the client watches and
//! tells us (`docs/index-design.md`, "no OS watcher"), and this module is what receives it.
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

use std::path::PathBuf;

use lsp_types::{DidChangeWatchedFilesParams, FileChangeType};

use crate::context::{ServerContextSnapshot, UpdateEvent};
use crate::util::uri_to_file_path;

pub async fn on_did_change_watched_files(
    context: ServerContextSnapshot,
    params: DidChangeWatchedFilesParams,
) -> Option<()> {
    let _ = context
        .update_tx()
        .send(UpdateEvent::WatchedFilesChanged(params));
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
            .update_session(move |session| session.changed(changes))
            .await;

        let Some(response) = response else {
            // No workspace yet: the events are about a project nobody has opened.
            return Some(());
        };

        // The events queued work — re-reading the files that include what changed, or the whole project after a
        // configuration change — and the pump is what reads it.
        context.analysis().wake();

        if response.everything {
            log::info!("the whole project has to be read again");
        } else {
            let paths: Vec<PathBuf> = response.reindex.clone();
            if !context.lsp_features().supports_pull_diagnostic() {
                context
                    .file_diagnostic()
                    .add_files_diagnostic_task(paths, crate::context::DEFAULT_DIAGNOSTIC_INTERVAL)
                    .await;
            }
        }
    }

    for uri in deleted {
        if !uri_to_file_path(&uri).is_some_and(|path| path.exists()) {
            context.file_diagnostic().clear_push_file_diagnostics(uri);
        }
    }

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
