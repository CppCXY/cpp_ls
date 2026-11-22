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
//! Clients with dynamic registration provide the events; otherwise the server uses a native watcher.

use log::info;
use lsp_types::{
    DidChangeWatchedFilesRegistrationOptions, FileSystemWatcher, GlobPattern, Registration,
    RegistrationParams, Unregistration, UnregistrationParams, WatchKind,
};

use crate::context::{ClientProxy, ServerContextSnapshot};

/// The registration id, so that a reload can replace the previous registration rather than add another one.
const WATCH_FILES_REGISTRATION_ID: &str = "cpp_ls_watched_files";

/// **The globs a client is asked to report on, built from the analysis's own lists.**
///
/// Braces are part of the LSP glob dialect (`GlobPattern::String` is matched with the client's own implementation, which
/// follows the protocol's grammar).
///
/// The extensions come from `cpp_code_analysis` rather than from a copy here, because a copy is a second list to drift:
/// this one had no `.cu`/`.cuh` (a CUDA project's own sources, which the analysis scans for) and no `cppm`/`ixx`
/// (`docs/language-server-review.md` P1-6). A client using its own watcher, as a client that registers dynamically does,
/// then reported nothing for a `.cu` file whose changes the session kept serving an old reading of.
///
/// The configuration names are the same arrangement on the other side of the question: they come from
/// `cpp_code_analysis::CONFIGURATION_FILE_NAMES`, so "a file the client tells us about" and "a file that means
/// reconfigure" are one list.
fn watched_globs() -> Vec<String> {
    let extensions: Vec<&str> = cpp_code_analysis::source_extensions()
        .iter()
        .chain(cpp_code_analysis::cuda_source_extensions())
        .copied()
        .collect();

    let mut globs = vec![format!("**/*.{{{}}}", extensions.join(","))];
    globs.extend(
        cpp_code_analysis::CONFIGURATION_FILE_NAMES
            .iter()
            .map(|name| format!("**/{name}")),
    );
    globs
}

pub async fn register_files_watch(context: ServerContextSnapshot) {
    if !context
        .lsp_features()
        .supports_dynamic_watched_files_registration()
    {
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

    let globs = watched_globs();
    register(context.client(), &globs);
    info!("asked the client to watch {} globs", globs.len());
}

fn register(client: &ClientProxy, globs: &[String]) {
    // Unregister first: a reload registers again, and two registrations of the same globs would report every event
    // twice — which is not a correctness problem for the analysis (the batch is idempotent) but is a doubled cost
    // in every event the user makes.
    unregister(client);

    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: globs
            .iter()
            .map(|glob| FileSystemWatcher {
                glob_pattern: GlobPattern::String(glob.clone()),
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

#[cfg(test)]
mod tests {
    use super::watched_globs;

    /// **The client is asked to watch what the analysis reads.**
    ///
    /// The glob list used to be a copy of the extension list, and the copy had no `.cu`/`.cuh` — so a client using its
    /// own watcher, which is what dynamic registration is for, reported nothing when a CUDA source changed
    /// (`docs/language-server-review.md` P1-6). The two lists are one now, and this pins that the globs cover **every**
    /// extension the analysis scans, CUDA's included, rather than the particular set that happened to be written down.
    #[test]
    fn the_watched_globs_cover_every_extension_the_analysis_reads() {
        let globs = watched_globs().join(" ");
        for extension in cpp_code_analysis::source_extensions()
            .iter()
            .chain(cpp_code_analysis::cuda_source_extensions())
        {
            assert!(
                globs.contains(&format!(",{extension},"))
                    || globs.contains(&format!("{{{extension},"))
                    || globs.contains(&format!(",{extension}}}")),
                "{extension} is in the glob list: {globs}"
            );
        }
        for name in cpp_code_analysis::CONFIGURATION_FILE_NAMES {
            assert!(
                globs.contains(&format!("**/{name}")),
                "{name} is a configuration file the client watches: {globs}"
            );
        }
    }
}
