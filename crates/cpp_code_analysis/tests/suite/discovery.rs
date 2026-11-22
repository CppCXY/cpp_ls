//! **The compile database, as a session sees it** — CURl's shape: CMake, forward slashes, a build directory.
//!
//! `parse_compile_commands` reads the spelling (see `cpp_toolchain`'s `compile_databases` fixtures), and this is the
//! wiring above it: the database is found, it is part of [`Session::config`], and a file it names is *in* the database
//! without being a source the project scan knows. The last part is what a `sweep` asks to decide whether a file is
//! measured at all, and on curl it answered "no" for every file — with the parser's half proven, the defect is here.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// A temporary project tree, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let root =
            std::env::temp_dir().join(format!("cppls-database-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the tree is created");
        Tree { root }
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the parent is created");
        }
        std::fs::write(&path, text).expect("the file is written");
        path
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

/// **A CMake database makes a file a file the build compiles**, and its flags are the file's configuration.
#[test]
fn a_cmake_database_is_found_and_its_files_are_in_it() {
    let tree = Tree::new("cmake");
    let root = tree.root.clone();
    let source = tree.write("lib/url.c", "int x;\n");
    // The CMake spelling: forward slashes, `directory` the build tree, an absolute source.
    let database = format!(
        r#"[ {{ "directory": "{build}", "command": "gcc -DBUILDING_LIBCURL -I{root}/include -c {root}/lib/url.c", "file": "{root}/lib/url.c" }} ]"#,
        build = root.join("build").display().to_string().replace('\\', "/"),
        root = root.display().to_string().replace('\\', "/"),
    );
    tree.write("compile_commands.json", &database);

    let session = session_over(&root);
    assert!(
        session.config().is_in_the_database(&source),
        "the database was found and the file is in it: {:?}",
        session.discovery().problems
    );
    let config = session.config_for(&source);
    assert!(
        config.is_in_the_database(&source),
        "the per-file configuration knows it too"
    );
}

/// **A real project, opened the way the tool opens one** — `E:\corpora\curl`, when this machine has it. The tool
/// reports every one of its files as "not in the database", while the database itself reads whole and its paths match
/// (see `cpp_toolchain`'s `compile_databases` fixtures). This opens it both ways the tool could: the root as given and
/// the root canonicalized.
#[test]
fn a_real_project_is_in_its_database() {
    let root = PathBuf::from(r"E:\corpora\curl");
    let source = root.join("lib").join("url.c");
    if !root.join("compile_commands.json").is_file() || !source.is_file() {
        return; // the corpus is not on this machine
    }

    let session = session_over(&root);
    assert!(
        session.config().is_in_the_database(&source),
        "the plain root: {:?}",
        session.discovery().problems
    );

    let canonical = root.canonicalize().expect("the corpus is readable");
    let session = session_over(&canonical);
    assert!(
        session.config().is_in_the_database(&source),
        "the canonical root: {:?}",
        session.discovery().problems
    );
}

#[test]
fn a_file_the_database_does_not_name_is_not_in_it() {
    let tree = Tree::new("cmake-negative");
    let root = tree.root.clone();
    let named = tree.write("lib/url.c", "int x;\n");
    let other = tree.write("lib/aix.c", "int y;\n");
    let database = format!(
        r#"[ {{ "directory": "{build}", "command": "gcc -c {named}", "file": "{named}" }} ]"#,
        build = root.join("build").display().to_string().replace('\\', "/"),
        named = named.display().to_string().replace('\\', "/"),
    );
    tree.write("compile_commands.json", &database);

    let session = session_over(&root);
    assert!(session.config().is_in_the_database(&named));
    assert!(
        !session.config().is_in_the_database(&other),
        "a file the database does not name is not in it"
    );
}

/// **The same file under the verbatim spelling.** A Session's files are keyed with the \\?\ prefix on Windows —
/// that is what std::fs::canonicalize answers — while a compile_commands.json writes the plain one. A membership
/// test that cannot cross the two says "not in the database" for every file of a project whose paths went through the
/// canonical form, which is what a sweep of curl reported: 0 files measured, 188 not in the database.
#[test]
fn a_verbatim_path_names_the_same_file() {
    let tree = Tree::new("verbatim");
    let root = tree.root.clone();
    let named = tree.write("lib/url.c", "int x;\n");
    let database = format!(
        r#"[ {{ "directory": "{build}", "command": "gcc -c {named}", "file": "{named}" }} ]"#,
        build = root.join("build").display().to_string().replace('\\', "/"),
        named = named.display().to_string().replace('\\', "/"),
    );
    tree.write("compile_commands.json", &database);

    let session = session_over(&root);
    let verbatim = PathBuf::from(format!(r"\\?\{}", named.display()));
    assert!(
        session.config().is_in_the_database(&verbatim),
        "the verbatim spelling is the same file: {:?}",
        verbatim
    );
}
