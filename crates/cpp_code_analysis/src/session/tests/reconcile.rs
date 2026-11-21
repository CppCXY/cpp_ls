//! **A reconcile** — the question a watcher that lost events asks the filesystem.
//!
//! The server used to answer a lost-event report by re-opening the whole workspace: a compiler run, a whole-project
//! index and a whole-program read, which on a Linux kernel tree is ten seconds. Measured on a real checkout, the log
//! held **332 overflow reports and four reloads in fifty seconds**, and each reload's program commit held the write
//! lock for five seconds — the window in which every diagnostic pull waited 4.9 s.
//!
//! What replaces it is [`Session::reconcile`]: list the project's files again, `stat` the ones the session already has,
//! and turn whatever moved into the same index work a client's own change events produce. These tests are about the
//! three answers it has to get right — nothing moved, a file moved, a file appeared or disappeared — and about the
//! property that makes it worth doing: it must not re-read a file whose bytes did not change.

use crate::file::paths::DiskFiles;
use crate::include::config::CompilerConfig;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};
use crate::symbol::Known;

use super::*;

/// A project on disk with these files, opened and indexed — the state a reconcile starts from.
fn indexed(name: &str, files: &[(&str, &str)]) -> (Project, Session<DiskFiles>) {
    let project = Project::new(name);
    for (path, text) in files {
        project.write(path, text);
    }
    let mut session = Session::with_config(
        &project.root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project.root),
        CompilerConfig::default(),
    );
    session.index_everything();
    (project, session)
}

/// **Nothing moved is the ordinary answer**, and it has to be cheap and empty: a reconcile runs after every storm, and
/// a rescan that re-reads the project because one event was lost is the thing this replaces.
#[test]
pub(crate) fn a_reconcile_with_nothing_changed_does_nothing() {
    let (project, mut session) = indexed(
        "reconcile-quiet",
        &[
            ("base.h", "int base(void);\n"),
            (
                "first.c",
                "#include \"base.h\"\nint first(void) { return base(); }\n",
            ),
        ],
    );

    let nothing = session.reconcile();
    assert!(
        nothing.is_empty(),
        "no file moved, so a reconcile is no work at all: {nothing:?}"
    );
    assert_eq!(
        project.relative(session.project_files()),
        ["base.h", "first.c"]
    );
}

/// **A rewritten file, a deleted file, and a new one**, in one call — the three answers a lost-event report needs.
///
/// The delete is what a range of events can hide: a file the client deleted while the watcher was overflowing has no
/// event to replay, so the only thing that can notice is a listing. The create is the same argument the other way.
#[test]
pub(crate) fn a_reconcile_reads_what_moved_and_only_that() {
    let (project, mut session) = indexed(
        "reconcile-moves",
        &[
            ("base.h", "int base(void);\n"),
            (
                "first.c",
                "#include \"base.h\"\nint first(void) { return base(); }\n",
            ),
            (
                "second.c",
                "#include \"base.h\"\nint second(void) { return base(); }\n",
            ),
        ],
    );
    let base = project.root.join("base.h");
    let first = project.root.join("first.c");
    let second = project.root.join("second.c");
    assert!(
        session.is_indexed(&second),
        "the session starts from a project that is read"
    );

    // One rewritten (different length, so the stamp moves even on a filesystem with a coarse clock), one deleted, one
    // added — exactly what a `make` in the tree looks like from the index's side.
    project.write(
        "first.c",
        "#include \"base.h\"\nint first_renamed(void) { return base(); }\n",
    );
    std::fs::remove_file(&second).expect("the fixture file is removable");
    let third = project.write(
        "third.c",
        "#include \"base.h\"\nint third(void) { return base(); }\n",
    );

    let moved = session.reconcile();
    assert!(
        moved.reindex.iter().any(|path| path == &first),
        "the rewritten file is re-read: {moved:?}"
    );
    assert!(
        moved.reindex.iter().any(|path| path == &third),
        "the new file is read: {moved:?}"
    );
    assert!(
        moved.forgotten.contains(&second),
        "the deleted file is forgotten: {moved:?}"
    );
    assert!(
        !moved.reindex.iter().any(|path| path == &base),
        "the header did not move, so it must not be re-read: {moved:?}"
    );

    // And the project list follows the disk: a deleted file that stayed in it would be asked about on every reconcile
    // for ever, and a new one that did not join it would never be read again.
    assert!(session.contains_project_file(&third));
    assert!(!session.contains_project_file(&second));

    session.index_everything();
    assert!(
        matches!(
            session.index().definition("first_renamed", &first),
            Known::Yes(_)
        ),
        "the re-read file answers with its new text"
    );
    assert!(
        matches!(session.index().definition("third", &third), Known::Yes(_)),
        "the new file is part of the project now"
    );
    assert!(
        !session.is_indexed(&second),
        "and the deleted one is not in the index any more"
    );
}

/// **A buffer is the text, so the disk's mtime is not evidence about it.**
///
/// The client is the authority for a document it has open: an editor writes the buffer on save, and a reconcile that
/// re-read the disk for an open file would answer about text nobody is looking at.
#[test]
pub(crate) fn a_reconcile_leaves_an_open_buffer_alone() {
    let (project, mut session) = indexed(
        "reconcile-open",
        &[("open.c", "int before(void) { return 0; }\n")],
    );
    let path = project.root.join("open.c");
    session.did_open(&path, "int in_the_buffer(void) { return 0; }\n");
    session.index_everything();

    // The disk changes underneath the buffer — what a `git checkout` in a terminal looks like to a server.
    std::fs::write(&path, "int on_the_disk(void) { return 0; }\n")
        .expect("the fixture is writable");

    let moved = session.reconcile();
    assert!(
        !moved.reindex.iter().any(|queued| queued == &path),
        "an open document is re-read when the client says so, not when the disk moves: {moved:?}"
    );
    assert!(
        matches!(
            session.index().definition("in_the_buffer", &path),
            Known::Yes(_)
        ),
        "the buffer is still the text the index was built from"
    );
}
