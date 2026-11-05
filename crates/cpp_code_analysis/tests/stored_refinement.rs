//! **A refinement is served from the disk on the next start** — the record that makes lazy refinement affordable
//! across sessions.
//!
//! A refinement is L2: the file as its closure renders it, so a class behind another file's macro body has a scope.
//! It costs ~45 ms on a real project and it is asked for lazily, on the first request about a file — which means
//! every *session* used to pay it again, because the cache stored the refined summary and a summary cannot carry the
//! cooked reading ("a summary is not a rendering", `docs/refinement.md` §3.2).
//!
//! The second file below is the regression test: the same project, a fresh session, and the refinement has to come
//! off the disk.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test stored_refinement -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A scratch project that removes itself.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cpp_ls-stored-refinement")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Project { root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, text).expect("the fixture writes");
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // `CPPLS_KEEP_FIXTURE=1` leaves the directory behind, for reading a failure.
        if std::env::var_os("CPPLS_KEEP_FIXTURE").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn a_session(root: &Path) -> Session<DiskFiles> {
    Session::with_config(
        root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(root),
        CompilerConfig::default(),
    )
}

const OPEN_H: &str = "#pragma once\n#define _STD_BEGIN namespace std {\n#define _STD_END }\n";
const WIDGET_H: &str = "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; };\n_STD_END\n";

/// **The refinement a first session built is the one a second session serves.**
///
/// Both sessions are given the same project — including the `.cppls` cache directory the first one wrote — and the
/// second one must not rebuild the refinement. The counter is the assertion: `stored_refinements_served` is zero for
/// a cache that cannot answer, and one for this fixture, and a timing assertion would be flaky where a count is not.
#[test]
fn a_second_session_serves_the_stored_refinement() {
    let project = Project::new("served");
    project.write("open.h", OPEN_H);
    let widget = project.write("widget.h", WIDGET_H);
    let main = project.write("main.cpp", "#include \"widget.h\"\n");

    // **The first session does the work.** `index_everything` reads the files and asks for their refinement; the
    // answer is written to the cache directory as a refined summary *and* a cooked reading.
    let mut first = a_session(project.root());
    first.index_everything();
    assert!(
        first.is_refined(&widget),
        "the fixture's `widget.h` has to be refined for this test to be about serving one"
    );
    assert!(
        matches!(
            first.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "…and the refinement is what makes the macro body's scope visible"
    );
    let served_by_the_first = first.stats().stored_refinements_served;
    drop(first);

    // **The second session, over the same directory.** Nothing has changed, so nothing has to be rebuilt.
    let mut second = a_session(project.root());
    second.index_everything();

    assert!(
        second.stats().stored_refinements_served > served_by_the_first,
        "the second session served the refinement the first one stored: {:?} vs {:?}",
        second.stats(),
        served_by_the_first
    );
    assert!(
        second.is_refined(&widget),
        "and the file is refined — served is refined, or the count would be a lie"
    );
    assert!(
        matches!(
            second.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "with the scope the refinement gives it: a served reading is a *reading*, not just a mark"
    );
}

/// **A changed file is not served** — the key's validity check, which is the whole safety of serving anything.
///
/// A cooked reading is a function of its file's text and of its whole closure's content, and both are in the key. The
/// test edits the file between the two sessions: the record is still on disk, its key no longer describes this text,
/// and the second session has to rebuild. A cache that answered here would answer about the text the reader replaced,
/// which is the one failure mode a cache this size must not have.
#[test]
fn a_changed_file_is_not_served_from_the_cache() {
    let project = Project::new("changed");
    project.write("open.h", OPEN_H);
    let widget = project.write("widget.h", WIDGET_H);
    let main = project.write("main.cpp", "#include \"widget.h\"\n");

    let mut first = a_session(project.root());
    first.index_everything();
    assert!(first.is_refined(&widget));
    drop(first);

    // **The edit**: one member more, the directives untouched — an ordinary keystroke.
    std::fs::write(
        &widget,
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; int height; };\n_STD_END\n",
    )
    .expect("the fixture rewrites");

    let mut second = a_session(project.root());
    second.index_everything();
    // **One file is served and it is not this one**: `main.cpp` did not change, so its record still describes its
    // text, while `widget.h` did — its record is a miss however similar the two readings are.
    assert_eq!(
        second.stats().stored_refinements_served,
        1,
        "only the file whose text did not change may be served"
    );
    assert!(
        second.is_refined(&widget),
        "the pass rebuilt it, so the file is refined again"
    );
    let rebuilt = second
        .index()
        .cooked_declarations(&widget)
        .expect("the rebuilt reading is the file's cooked one");
    assert!(
        rebuilt.iter().any(|fact| fact.name == "height"),
        "and it is the edited text's reading: {rebuilt:?}"
    );
    assert!(
        matches!(
            second.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "and the rebuilt reading still has the macro body's scope"
    );
}
