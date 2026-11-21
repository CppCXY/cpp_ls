//! **`[completion] max_items` is honoured, and a cut list says so** — the budget a project can set.
//!
//! The list a completion answers with is a **payload** bound rather than a UI one: a client renders ten rows and
//! receives all of them, and an untruncated `std::` is 12 867 items. What makes the bound harmless is that a cut list
//! reports itself (`isIncomplete: true`), so the client asks again on the next keystroke and the prefix filter — applied
//! in the index before anything is cloned — narrows the answer exactly:
//!
//! ```text
//! `std::`          1000 items, truncated true,  53 ms
//! `std::s`          167 items, truncated false,  7 ms
//! `std::str`         38 items, truncated false,  6 ms
//! `std::string`       6 items, truncated false,  6 ms
//! ```
//!
//! Measured on the reader's project. This test is the configuration half: the same query with and without the file.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// A scratch project that removes itself.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cpp_ls-completion-config")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Project { root }
    }

    fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, text).expect("the file is written");
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const HEADER: &str = "\
namespace wide {
struct One {};
struct Two {};
struct Three {};
struct Four {};
struct Five {};
struct Six {};
struct Seven {};
struct Eight {};
}
";

const SOURCE: &str = "\
#include \"wide.h\"
void f() {
    wide::
}
";

/// Ask the completion at the end of `wide::` and answer `(items, truncated)`.
fn completions(project: &Project) -> (usize, bool) {
    let main = project.write("main.cpp", SOURCE);
    let header = project.write("wide.h", HEADER);
    let mut session = Session::open(
        project.root().to_path_buf(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(project.root()),
    );
    session.did_open(&main, SOURCE);
    session.add_project_files([main.clone(), header]);
    session.index_everything();
    let view = session.view_of_the_file(&main).expect("a view");
    let at = SOURCE.find("wide::").expect("the fixture") + "wide::".len();
    let offered = session.completions(&view, FileOffset::new(at));
    (offered.items.len(), offered.truncated)
}

#[test]
fn the_completion_budget_is_a_project_setting() {
    // **Without the file**: every name, and nothing cut.
    let unconfigured = Project::new("unconfigured");
    let (count, truncated) = completions(&unconfigured);
    assert_eq!(count, 8, "all eight names are offered, and only those");
    assert!(!truncated, "and the list is complete");

    // **With `max_items = 3`**: three of them, and the cut is reported.
    let configured = Project::new("configured");
    configured.write(".cppls.toml", "[completion]\nmax_items = 3\n");
    // **The file parses first**, so a failure below says which half is wrong.
    let report = cpp_code_analysis::project::load_config(&DiskFiles, configured.root(), None);
    assert_eq!(
        report.config.completion.max_items,
        Some(3),
        "`[completion] max_items` is a section the parser reads: {:?}",
        report.problems
    );
    let (count, truncated) = completions(&configured);
    assert_eq!(count, 3, "the project asked for three");
    assert!(
        truncated,
        "and a cut list reports itself, so the client asks again as the reader types"
    );
}
