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
use crate::context::{DiagnosticService, ProgressTask, ServerContextSnapshot, get_client_id};
use crate::handlers::register_files_watch;
use crate::logger::init_logger;
use crate::util::uri_to_file_path;

pub use client_config::{ClientConfig, get_client_config};

/// How many files one background slice reads before it lets the runtime run something else.
///
/// A slice is a transaction against the analysis: it takes the write lock once and gives it back, so a query
/// arriving mid-index waits for a slice and not for a project. Sixteen files is a few milliseconds of parsing
/// (measured in `docs/index-design.md`) — short enough that a keystroke does not feel it, long enough that the
/// indexing does not spend its time taking locks.
const INDEX_SLICE: usize = 16;

/// How long the pump sleeps when there is nothing to read, before looking again.
///
/// A safety net rather than the mechanism: an edit *wakes* the pump (`AnalysisState::wake`), and a wake-up that
/// arrives while the pump is busy is remembered. This is the backstop for one arriving in the instant between the
/// queue being found empty and the wait starting — one lock acquisition a second, against an index that would
/// otherwise stay stale for the rest of the session if that ever happened.
const IDLE_WAIT: std::time::Duration = std::time::Duration::from_millis(1000);

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
    let (root, filter) = {
        let workspace_manager = context.workspace_manager().lock().await;
        (
            workspace_manager.root()?.to_path_buf(),
            workspace_manager.watch_filter()?,
        )
    };

    context
        .status_bar()
        .create_progress_task(ProgressTask::LoadWorkspace)
        .await;

    // The compiler is run here (`Session::open` asks it for its search paths), so this is the one slow call in the
    // handshake, and it happens on the blocking pool rather than on the runtime's own thread.
    context.analysis().open(root.clone(), filter).await;

    // The open buffers are read **after** the session exists, and the order is the point: a client that restarted
    // with unsaved files would otherwise have the analysis answer about the disk, and a change that arrived while
    // the session was being built (the update queue is a separate task) is in this list because it is read now.
    let open_files = {
        let workspace_manager = context.workspace_manager().lock().await;
        workspace_manager.workspace_open_files()
    };

    if !open_files.is_empty() {
        let count = open_files.len();
        context
            .analysis()
            .update_session(move |session| {
                for (uri, text) in &open_files {
                    if let Some(path) = uri_to_file_path(uri) {
                        session.did_open(path, text);
                    }
                }
            })
            .await;
        log::info!("re-marked {count} open buffers as the text the analysis reads");
    }

    log::info!("analysing {}", root.display());
    Some(root)
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
        let version = context
            .workspace_manager()
            .lock()
            .await
            .workspace_version();
        let mut first_pass = true;

        loop {
            // A reload bumps the version and starts a pump of its own: this one stops rather than racing it for
            // the write lock, because its idea of what the project is is the one that was replaced.
            if context
                .workspace_manager()
                .lock()
                .await
                .workspace_version()
                != version
            {
                log::debug!("the workspace was reloaded; this indexing pass stops");
                return;
            }

            let Some(pending) = context
                .analysis()
                .update_session(|session| {
                    session.advance(INDEX_SLICE);
                    session.pending()
                })
                .await
            else {
                log::warn!("the workspace was closed while it was being indexed");
                return;
            };

            if pending > 0 {
                context.status_bar().update_progress_task(
                    ProgressTask::LoadWorkspace,
                    None,
                    Some(format!("{pending} files to read")),
                );

                // Yield between slices: the runtime is also serving requests, and a loop that never awaits would
                // hold the worker it runs on.
                tokio::task::yield_now().await;
                continue;
            }

            // Nothing to read. The first time that happens the project is loaded, and the diagnostics that were
            // waiting for it are published — after that the loop simply waits for the next edit.
            if first_pass {
                first_pass = false;
                context.status_bar().finish_progress_task(
                    ProgressTask::LoadWorkspace,
                    Some("Workspace loaded".to_string()),
                );
                log::info!("the workspace is indexed");

                publish_workspace_diagnostics(&context).await;
            }

            context.analysis().wait_for_work(IDLE_WAIT).await;
        }
    });
}

/// The first full diagnostic pass, once the index holds something.
///
/// Published rather than pulled, because a client that supports pull diagnostics asks for a file's diagnostics when
/// it shows that file — and there is nothing to push to it. A client that does not would otherwise see an empty
/// problem list until it opened every file in the project.
async fn publish_workspace_diagnostics(context: &ServerContextSnapshot) {
    let file_diagnostic: &DiagnosticService = context.file_diagnostic();
    if context.lsp_features().supports_pull_diagnostic() {
        if context.lsp_features().supports_refresh_diagnostic() {
            context.client().refresh_workspace_diagnostics();
        }
        return;
    }

    file_diagnostic.add_workspace_diagnostic_task(0).await;
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
