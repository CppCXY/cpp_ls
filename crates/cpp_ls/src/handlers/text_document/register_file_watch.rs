//! # Telling the client what to watch
//!
//! `workspace/didChangeWatchedFiles` is only useful if the client knows which files to report, and the LSP way to
//! say so is a **dynamic registration**: `client/registerCapability` with a list of globs.
//!
//! ```text
//! **/*.{c,cc,cpp,cxx,h,hh,hpp,hxx,ipp,inl,tpp,tcc}    the sources and headers this analysis reads
//! **/compile_commands.json                          the file that configures it
//! **/*.cppm, **/*.ixx                               module interfaces (a C++20 project's own kind of source)
//! ```
//!
//! # Why the client watches and this server does not
//!
//! A server-side watcher (`notify`, inotify, a poll loop) is a second opinion about a filesystem the client is
//! already watching, and a worse one: it costs a thread and a recursive watch per root, it has to debounce a
//! temporary-file rename that the client performed and understands, and on a large checkout it is the difference
//! between an idle server and a busy one. The decision is `docs/index-design.md`'s ("no OS watcher"), and this
//! module is the other half of it: what the client cannot be asked to do, this server does not do either.
//!
//! A client that does not support dynamic registration gets no registration and no events; it still gets
//! diagnostics for every file it *opens*, which is the part that does not need a watcher. That is a real limitation
//! and it is logged rather than hidden.

use log::{info, warn};
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, Registration,
    RegistrationParams, Unregistration, UnregistrationParams, WatchKind,
};

use crate::context::{ClientProxy, ServerContextSnapshot};

/// The registration id, so that a reload can replace the previous registration rather than add another one.
const WATCH_FILES_REGISTRATION_ID: &str = "cpp_ls_watched_files";

/// The globs a client is asked to report on.
///
/// Braces are part of the LSP glob dialect (`GlobPattern::String` is matched with the client's own implementation,
/// which follows the protocol's grammar), and this is the same extension list the analysis scans a project for —
/// one list, so that "what the client tells us about" and "what the scan picks up" cannot drift apart.
const WATCHED_GLOBS: &[&str] = &[
    "**/*.{c,cc,cpp,cxx,c++,h,hh,hpp,hxx,h++,ipp,inl,tpp,tcc}",
    "**/*.{cppm,ixx}",
    "**/compile_commands.json",
];

pub async fn register_files_watch(context: ServerContextSnapshot) {
    if !context
        .lsp_features()
        .supports_dynamic_watched_files_registration()
    {
        warn!(
            "the client cannot be asked to watch files; changes made outside the editor will not be noticed"
        );
        return;
    }

    if context
        .workspace_manager()
        .lock()
        .await
        .watch_filter()
        .is_none()
    {
        // Nothing to watch yet. `initialized` registers again once a folder is named, which is the only moment the
        // globs mean anything.
        return;
    }

    register(context.client());
    info!("asked the client to watch {} globs", WATCHED_GLOBS.len());
}

fn register(client: &ClientProxy) {
    // Unregister first: a reload registers again, and two registrations of the same globs would report every event
    // twice — which is not a correctness problem for the analysis (the batch is idempotent) but is a doubled cost
    // in every event the user makes.
    unregister(client);

    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: WATCHED_GLOBS
            .iter()
            .map(|glob| FileSystemWatcher {
                glob_pattern: GlobPattern::String((*glob).to_string()),
                kind: Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete),
            })
            .collect(),
    };

    client.dynamic_register_capability(RegistrationParams {
        registrations: vec![Registration {
            id: WATCH_FILES_REGISTRATION_ID.to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(
                serde_json::to_value(options).expect("the registration options serialize"),
            ),
        }],
    });
}

fn unregister(client: &ClientProxy) {
    client.dynamic_unregister_capability(UnregistrationParams {
        unregisterations: vec![Unregistration {
            id: WATCH_FILES_REGISTRATION_ID.to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
        }],
    });
}
