//! # WorkspaceState — the roots, the buffers, and what the client asked for
//!
//! Pure data. [`crate::context::WorkspaceManager`] orchestrates reloads and re-indexing, and this is the state it
//! reads and writes: which directories the client opened, which buffers it has open, and the configuration it
//! answered `workspace/configuration` with.
//!
//! # Why the roots are a list and the session has one
//!
//! A client may open several folders, and a [`Session`](cpp_code_analysis::Session) is opened over **one** root —
//! that is the engine's shape, because the compile database, the cache directory and the configuration all live
//! under one project. So the first root is the one the analysis runs over, and the others are recorded here and
//! reported at startup rather than silently dropped. `docs/ls-architecture.md` §5 carries the multi-root item; the
//! honest state today is "one folder is analysed, and the log says which".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{PathPattern, WatchFilter};
use lsp_types::Uri;

use crate::handlers::ClientConfig;
use crate::util::uri_to_file_path;

#[derive(Debug)]
pub struct WorkspaceState {
    /// The directories the client opened, in the order it named them. The first is the session's root.
    pub roots: Vec<PathBuf>,
    /// The editor's open buffers, by URI, as the client sent them.
    pub open_files: HashMap<Uri, String>,
    pub client_config: ClientConfig,
    /// The client's `exclude` list, parsed once — see [`WorkspaceState::set_client_config`].
    excludes: Vec<PathPattern>,
}

impl WorkspaceState {
    pub fn new(client_config: ClientConfig) -> Self {
        let mut state = Self {
            roots: Vec::new(),
            open_files: HashMap::new(),
            client_config: ClientConfig::default(),
            excludes: Vec::new(),
        };
        state.set_client_config(client_config);
        state
    }

    /// The root the analysis runs over: the first folder the client opened.
    pub fn root(&self) -> Option<&Path> {
        self.roots.first().map(PathBuf::as_path)
    }

    pub fn set_roots(&mut self, roots: Vec<PathBuf>) {
        self.roots = roots;
    }

    /// Take the client's configuration, parsing the exclusions it names.
    ///
    /// The parse is here rather than in the builder that uses it because this is the layer with a log: a pattern a
    /// user mistyped is dropped with a line saying so, and the workspace still opens. That is the same division the
    /// engine makes everywhere (`CompileCommands::malformed` counts what it could not read instead of printing),
    /// and it is what keeps `cpp_code_analysis` free of a logger.
    pub fn set_client_config(&mut self, client_config: ClientConfig) {
        self.excludes = client_config
            .exclude
            .iter()
            .filter_map(|written| match PathPattern::new(written) {
                Ok(pattern) => Some(pattern),
                Err(error) => {
                    log::warn!("ignoring the exclude pattern {written:?}: {error}");
                    None
                }
            })
            .collect();

        self.client_config = client_config;
    }

    /// The client says a buffer is now open with this text, or that its text is now this.
    ///
    /// One method for `didOpen`, `didChange` and a `didSave` that carried text, because the state has one question
    /// about a buffer: which text is this path's. The protocol distinguishes the events; nothing here does.
    pub fn sync_open_file(&mut self, uri: Uri, text: String) {
        self.open_files.insert(uri, text);
    }

    /// The client closed a document: the path is the filesystem's again.
    pub fn close_open_file(&mut self, uri: &Uri) {
        self.open_files.remove(uri);
    }

    pub fn is_open_file(&self, uri: &Uri) -> bool {
        self.open_files.contains_key(uri)
    }

    /// The open buffers inside the workspace, which is what a session is told about when it opens.
    ///
    /// A buffer outside every root is left out: it is still the editor's text (`OpenDocuments` answers for it), but
    /// it is not what this project is made of, and a reload re-marks the project's buffers.
    pub fn workspace_open_files(&self) -> Vec<(Uri, String)> {
        self.open_files
            .iter()
            .filter(|(uri, _)| self.is_workspace_file(uri))
            .map(|(uri, text)| (uri.clone(), text.clone()))
            .collect()
    }

    /// Is this file the workspace's own — under one of its roots, and not excluded?
    ///
    /// Before the client has named a folder every file is "ours": the answer is used to decide what to *diagnose*,
    /// and refusing everything until a root arrives would silently drop the first edits of a session that is
    /// starting up. Once there are roots, a path outside all of them is not ours — a system header a jump landed in
    /// is read and indexed, but it is not the project's source and the project's diagnostics are not published
    /// against it.
    pub fn is_workspace_file(&self, uri: &Uri) -> bool {
        if self.roots.is_empty() {
            return true;
        }

        let Some(path) = uri_to_file_path(uri) else {
            return true;
        };

        self.contains(&path)
    }

    /// [`WorkspaceState::is_workspace_file`] for a path already in hand.
    ///
    /// The exclusion test is the **session's own filter**, not a second reading of the same patterns: the answer
    /// here decides what gets diagnosed and what a project-wide pass covers, and the filter decides what the
    /// analysis reads. Two implementations of "is this file ours" would be free to disagree, and the disagreement
    /// would show up as diagnostics for files the index never read.
    pub fn contains(&self, path: &Path) -> bool {
        if self.roots.is_empty() {
            return true;
        }

        if !self.roots.iter().any(|root| path.starts_with(root)) {
            return false;
        }

        !self
            .watch_filter()
            .is_some_and(|filter| filter.is_ignored(path))
    }

    /// The filter a session is opened with: the roots' cache and compile database, plus the client's exclusions.
    ///
    /// Nothing is excluded that the client did not name. A build tree is `build` in one project, `out` in another
    /// and `tmp` in a third, so guessing would drop a real project's sources the moment the guess was wrong — the
    /// engine's `WatchFilter` says the same thing about its own ignore list, and this is the caller that fills it.
    ///
    /// `None` when no folder has been named, because a session cannot be opened without a root — the caller that
    /// gets `None` answers "there is no workspace yet", which is the truth.
    pub fn watch_filter(&self) -> Option<WatchFilter> {
        let root = self.root()?;
        let mut filter = WatchFilter::new(root);

        for pattern in &self.excludes {
            filter = filter.ignore_pattern(pattern.clone());
        }

        Some(filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `file:` URI for a path written with `/`, which is what a client sends on every platform.
    fn uri(path: &str) -> Uri {
        format!("file:///{path}").parse().expect("a file URI")
    }

    /// The path a URI maps to, **as this platform spells it** — the tests compare what the conversion produces with
    /// what the state decided, so they cannot pass on one platform by spelling a path the other one would not.
    fn path_of(written: &str) -> PathBuf {
        uri_to_file_path(&uri(written)).expect("a file URI names a path")
    }

    fn state(roots: &[&str], exclude: &[&str]) -> WorkspaceState {
        let mut state = WorkspaceState::new(ClientConfig {
            exclude: exclude.iter().map(|entry| entry.to_string()).collect(),
            ..ClientConfig::default()
        });
        state.set_roots(roots.iter().map(|root| path_of(root)).collect());
        state
    }

    #[test]
    fn a_file_inside_a_root_is_the_workspaces_and_one_outside_it_is_not() {
        let state = state(&["p"], &[]);

        assert!(state.is_workspace_file(&uri("p/src/main.cpp")));
        assert!(
            !state.is_workspace_file(&uri("other/lib.h")),
            "a file outside every root is not this workspace's source"
        );
    }

    #[test]
    fn an_excluded_directory_is_not_the_workspaces() {
        // The reason a client's exclude list reaches this far: a build tree is full of generated files, and the
        // scan that indexes everything would otherwise spend its time on them.
        let state = state(&["p"], &["**/build/**", "generated/*.h"]);

        assert!(!state.contains(&path_of("p/build/generated.cpp")));
        assert!(!state.contains(&path_of("p/generated/api.h")));
        assert!(state.contains(&path_of("p/src/main.cpp")));

        let filter = state.watch_filter().expect("the root is named");
        assert!(filter.is_ignored(&path_of("p/build/generated.cpp")));
        assert!(!filter.is_ignored(&path_of("p/src/main.cpp")));
    }

    #[test]
    fn a_buffer_is_open_until_it_is_closed() {
        let mut state = state(&["p"], &[]);
        let path = uri("p/src/main.cpp");

        assert!(!state.is_open_file(&path));
        state.sync_open_file(path.clone(), "int x;\n".to_string());
        assert!(state.is_open_file(&path));
        assert_eq!(state.workspace_open_files().len(), 1);

        state.close_open_file(&path);
        assert!(!state.is_open_file(&path));
        assert!(state.workspace_open_files().is_empty());
    }
}
