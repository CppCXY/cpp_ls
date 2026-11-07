//! **Does `textDocument/implementation` cross a module boundary?** — `.cppm` declaration, `.cpp` definition.
//!
//! The reader's report: Go to Implementation on `Report::joined` in `report.cppm` found nothing. The facts were the
//! first half (fixed: a definition inside a namespace produced no fact at all, and then carried `Report` where the
//! interface carried `report::Report`); this is the second half — the handler's own scan, `definitions_of`, asked with a
//! `Session` over a directory that looks like the reader's project.
//!
//! A project directory rather than `MemoryFiles`, for the same reason `stored_refinement.rs` writes one: the handler
//! answers with `Location`s and the scan reads each candidate's text to decide whether a declaration has a body, and
//! that read goes through the provider the session was opened with.
//!
//! ```text
//! cargo test --release -p cpp_ls --test module_implementation
//! ```

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::implementation::definitions_of;

/// A scratch project that removes itself.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cpp_ls-module-implementation")
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a scratch directory");
        }
        std::fs::write(&path, text).expect("the file is written");
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_cppm_declaration_answers_the_cpp_that_defines_it() {
    let project = Project::new("cppm-to-cpp");
    let interface_text = "\
export module report;

namespace report {

export struct Report {
    void add(int line);
    int joined(int separator) const;
};

}
";
    let implementation_text = "\
module report;

namespace report {

void Report::add(int line) {
    line = line + 1;
}

int Report::joined(int separator) const {
    return separator;
}

}
";
    let interface = project.write("report.cppm", interface_text);
    let implementation = project.write("report.cpp", implementation_text);

    let mut session = Session::open(
        project.root().to_path_buf(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(project.root()),
    );
    session.add_project_files([interface.clone(), implementation.clone()]);
    session.index_everything();

    // **The fact the handler scans for, from both sides** — the declaration in the interface and the definition in the
    // implementation must name the same class, or no comparison can match them.
    for (path, expected) in [
        (&interface, "report::Report"),
        (&implementation, "report::Report"),
    ] {
        let summary = session.index().summary(path).expect("the file is indexed");
        let scopes: Vec<Option<String>> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name.as_str() == "joined")
            .map(|fact| fact.scope.clone())
            .collect();
        assert!(
            scopes
                .iter()
                .any(|scope| scope.as_deref() == Some(expected)),
            "{}: `joined` is filed under `{expected}`: {scopes:?}",
            path.display()
        );
    }

    // **And the handler's answer** — asked at the declaration, answered with the definition.
    let view = session
        .view_of_the_file(&interface)
        .expect("the interface has a view");
    let at = interface_text.find("joined").expect("the declaration") + 1;
    let definition = session.definition(&view, ReadingOffset::new(at));
    assert!(
        matches!(definition, Known::Yes(_)),
        "the declaration resolves: {definition:?}"
    );

    let found = definitions_of(&session, &view, at);
    assert!(
        found
            .iter()
            .any(|location| location.uri.path().ends_with("report.cpp")),
        "Go to Implementation answers `report.cpp`: {found:?}"
    );
}
