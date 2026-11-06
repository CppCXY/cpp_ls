//! **A refinement is asked for again when the text it was built from changes** — the question the lazy-refinement
//! design turns on, measured rather than assumed.
//!
//! A refinement (L2) is a file's reading **through its closure**: the macro bodies its includes define are expanded,
//! so `_STD_BEGIN` opens `namespace std` and the declarations inside it are scoped. The first pass (L1) parses the
//! file's own text with no environment, and phase 0 of `docs/indexing-design.md` stopped a start from refining the
//! project: a file is refined when a request **asks**, and the answer is kept.
//!
//! Kept *how long* is what this file asks. `Session::want_refinement` refuses when `Session::is_refined` says the
//! reading is already the refined one, and `is_refined` answers from three places — a session set, a store set, and
//! the summary's environment hash. An edit drops the summary and the cooked reading; if it does not drop the first
//! two, a file that has been refined once can never be refined again in the same session, and after the first
//! keystroke every answer is the shape-only reading.
//!
//! The fixture is the one `a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session` uses, so the
//! two tests say the same thing about the same reading: `open.h` defines the macro body, `widget.h` renders through
//! it, and a name inside that scope is only findable when the reading is the refined one.

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};
use std::path::{Path, PathBuf};

/// A scratch project that removes itself.
struct Project {
    root: PathBuf,
}

impl Project {
    fn new(name: &str) -> Project {
        let root = std::env::temp_dir()
            .join("cpp_ls-refinement")
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
        let _ = std::fs::remove_dir_all(&self.root);
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

/// **The reading survives one edit or it does not** — the whole of the question, on the fixture the session test
/// uses.
#[test]
fn a_refined_reading_is_asked_for_again_after_an_edit() {
    let project = Project::new("re-ask");
    project.write("open.h", OPEN_H);
    let widget = project.write("widget.h", WIDGET_H);
    let main = project.write("main.cpp", "#include \"widget.h\"\n");

    let mut session = a_session(project.root());
    // **The ask**: the reading this test is about is what a *request* produces.
    session.want_refinement(&widget);
    session.index_everything();

    // The reading this test is about: refined, so the macro body's scope is in the index.
    assert!(
        matches!(
            session.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "the fixture's `Widget` is behind `_STD_BEGIN`, which only the refined reading expands"
    );
    assert!(
        session.is_refined(&widget),
        "and `widget.h` is the file that was refined"
    );

    // **A body edit**: the directives are the same, the body changed. This is an ordinary keystroke, and the file is
    // re-read by the first pass — which is the shape-only reading until something refines it again.
    session.did_change(
        &widget,
        "#include \"open.h\"\n_STD_BEGIN\nstruct Widget { int size; int height; };\n_STD_END\n",
    );
    session.index_everything();

    assert!(
        session.is_refined(&widget),
        "`widget.h`'s refined reading is what the session had; the edit dropped its summary and its cooked \
         reading, so the refined mark has to go with them"
    );
    assert!(
        matches!(
            session.index().definition("std::Widget", &main),
            Known::Yes(_)
        ),
        "the edited file must be refined again, or every answer after the first keystroke is the shape-only \
         reading: `std::Widget` would be `NotDeclaredHere`"
    );
}
