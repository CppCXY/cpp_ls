//! **`[[library]]` and a target's `depends`** — a dependency outside the workspace, read through what it exports.
//!
//! ```toml
//! [[library]]
//! name     = "widgets"
//! path     = "E:/SDK/widgets"      # outside the workspace, on purpose
//! includes = ["include"]
//!
//! [[target]]
//! name    = "app"
//! sources = ["src"]
//! depends = ["widgets"]
//! ```
//!
//! What the test pins, in the order the feature would fail:
//!
//! 1. the library's **headers** are part of the index, and its **`.cpp` files are not**;
//! 2. a file of the depending target has the library's include directory on its search path;
//! 3. a type it uses without including the header is reported by `missing-include`, naming the library's header;
//! 4. after the include is added, the finding is gone.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// A scratch tree: a project and a library that is **not** under it, both removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let root = std::env::temp_dir()
            .join("cpp_ls-library")
            .join(format!("{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");
        Tree { root }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a scratch directory");
        }
        std::fs::write(&path, text).expect("the file is written");
        path
    }

    fn project(&self) -> PathBuf {
        self.root.join("app")
    }

    fn library(&self) -> PathBuf {
        self.root.join("sdk").join("widgets")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn session_over(project: &Path) -> Session<DiskFiles> {
    Session::open(
        project.to_path_buf(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(project),
    )
}

#[test]
fn a_library_is_read_through_its_headers_and_named_by_the_diagnostic() {
    let tree = Tree::new("headers-only");
    let header = tree.write(
        "sdk/widgets/include/widgets/widget.h",
        "struct WidgetsWidget { int size; };\n",
    );
    let implementation = tree.write(
        "sdk/widgets/src/widget.cpp",
        "int an_implementation_only_function() { return 1; }\n",
    );
    tree.write(
        "app/.cppls.toml",
        &format!(
            "[[library]]\n\
             name = \"widgets\"\n\
             path = \"{}\"\n\
             includes = [\"include\"]\n\
             \n\
             [[compile.target]]\n\
             name = \"app\"\n\
             sources = [\"src/**\"]\n\
             depends = [\"widgets\"]\n",
            tree.library().to_string_lossy().replace('\\', "/")
        ),
    );
    let source = tree.write(
        "app/src/use.cpp",
        "void f(WidgetsWidget* widget) { widget->size = 1; }\n",
    );
    let text = std::fs::read_to_string(&source).expect("the source reads");

    let mut session = session_over(&tree.project());
    session.did_open(&source, &text);
    session.index_everything();

    // **1. What the library contributes**: its header, and not its implementation.
    let indexed: Vec<String> = session
        .project_files()
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    assert!(
        indexed
            .iter()
            .any(|path| path.ends_with("widgets/widget.h")),
        "the library's header is part of the index: {indexed:?}"
    );
    assert!(
        !indexed
            .iter()
            .any(|path| path.ends_with("widgets/src/widget.cpp")),
        "and its implementation is not: {indexed:?}"
    );
    assert!(
        session.index().summary(&header).is_some(),
        "the header is read"
    );
    assert!(
        session.index().summary(&implementation).is_none(),
        "the implementation is not"
    );

    // **2. The target's search path carries the library.**
    let config = session.config_for(&source);
    assert!(
        config
            .include_paths
            .iter()
            .any(|path| path.directory.ends_with("widgets/include")
                || path.directory.ends_with("widgets\\include")),
        "`depends = [\"widgets\"]` puts its include directory on this file's path: {:?}",
        config
            .include_paths
            .iter()
            .map(|path| path.directory.clone())
            .collect::<Vec<_>>()
    );

    // **3. The diagnostic names the library's header.**
    let diagnostics = session.diagnostics(&source).expect("the file reads");
    let finding = diagnostics
        .checks
        .iter()
        .find(|finding| {
            finding.code == cpp_code_analysis::sema::check::DiagnosticCode::MissingInclude
        })
        .unwrap_or_else(|| panic!("a missing include is reported: {:?}", diagnostics.checks));
    assert_eq!(finding.name, "WidgetsWidget");
    assert!(
        finding.message.contains("widget.h"),
        "the header that declares it is named: {}",
        finding.message
    );

    // **4. With the include, nothing is missing** — and the file's own reading proves it, because the type now
    // resolves from a header this file reaches.
    let included = format!("#include \"widgets/widget.h\"\n{text}");
    session.did_change(&source, &included);
    session.index_everything();
    let after = session.diagnostics(&source).expect("the edited file reads");
    assert!(
        !after.checks.iter().any(|finding| finding.code
            == cpp_code_analysis::sema::check::DiagnosticCode::MissingInclude),
        "the include the fix would add is enough: {:?}",
        after.checks
    );
}
