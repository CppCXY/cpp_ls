//! **Fixtures** — a project in memory, its buffers, and the directory it is given

use crate::file::paths::FileProvider;
use crate::file::paths::MemoryFiles;
use crate::include::config::CompilerConfig;
use crate::index;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};
use std::path::{Path, PathBuf};

/// **A fixture directory no other run can be using** — the name, the process id, and a serial.
///
/// The fixtures in this file used to share one tree per name (`%TEMP%\cppls-session-disk\body-pass`), and
/// [`std::fs::remove_dir_all`]'s failure was ignored. A directory that could not be removed — a `.cppls` a previous run
/// left, held open by another process on Windows — then became **this** run's state: the store loaded a summary keyed
/// by a text the fixture had just rewritten, and the answer was the old one. Measured as a report of
/// `a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` failing on every rerun with
/// ``Unknown(NotDeclaredHere("studio::Widget"))`` — the summary under `<fixture>/.cppls` still said `std`, and nothing
/// in the run could clear it. A unique name makes the stale directory unreachable rather than merely unlikely, and
/// keeps two concurrent `cargo test` runs from deleting each other's files.
pub(crate) fn a_fresh_fixture_directory(parent: &str, name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir()
        .join(parent)
        .join(format!("{name}-{}-{serial}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// A project in memory: the files, the buffers in front of them, and the providers that join the two.
///
/// The fixture keeps the providers and hands each session a **clone**, which is what a caller outside a test
/// does too — a provider is a handle, so the fixture's copy and the session's copy read the same files, and a
/// test can go on looking at what the analysis read while the session owns its own chain.
pub(crate) struct Memory<'a> {
    pub(crate) files: &'a MemoryFiles,
    pub(crate) documents: OpenDocuments,
    pub(crate) providers: SessionFiles<MemoryFiles>,
    pub(crate) root: PathBuf,
}

impl<'a> Memory<'a> {
    pub(crate) fn new(name: &str, files: &'a MemoryFiles) -> Self {
        let root = a_fresh_fixture_directory("cppls-session-tests", name);

        let documents = OpenDocuments::new();
        let providers = SessionFiles::new(documents.clone(), files.clone());

        Memory {
            files,
            documents,
            providers,
            root,
        }
    }

    pub(crate) fn session(&self) -> Session<MemoryFiles> {
        self.session_with(CompilerConfig::default())
    }

    /// A session over the same files, with the configuration the caller states — how a test reads a **CUDA** project
    /// (`CompilerConfig::with_language_family`), or one for a particular compiler.
    pub(crate) fn session_with(&self, config: CompilerConfig) -> Session<MemoryFiles> {
        Session::with_config(
            &self.root,
            self.providers.clone(),
            WatchFilter::new(&self.root),
            config,
        )
    }

    /// The text the analysis would read for a path — the buffer if there is one.
    pub(crate) fn text(&self, path: &str) -> Option<String> {
        self.documents
            .text(path)
            .or_else(|| self.files.read(Path::new(path)))
    }
}

impl Drop for Memory<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The paths of a run of steps, for an assertion about the order rather than about the work.
pub(crate) fn paths(steps: &[index::Step]) -> Vec<String> {
    steps
        .iter()
        .map(|step| step.path.to_string_lossy().replace('\\', "/"))
        .collect()
}

// -------------------------------------------------------------------------------------------
// The buffer is the text
// -------------------------------------------------------------------------------------------
