//! **A one-file fixture, cooked, with its declarations printed** — the diagnosis for the hover regression S1's
//! no-walk experiment caused: a class that declares `call` twice (once `const`) must produce two facts.
//!
//! ```text
//! cargo test -p cpp_code_analysis --test cook_fixture_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::include::config::CompilerConfig;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: cooks one fixture and prints the facts"]
fn two_overloads_of_one_name() {
    let root: PathBuf = std::env::temp_dir().join("cppls-cook-fixture-probe");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "struct Foo {\n\
                  template <typename Fn> void call(Fn &&fn) {}\n\
                  template <typename Fn> void call(Fn &&fn) const {}\n\
                  };\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.index_everything();
    let reading = session.cook(&file).expect("the file cooks");
    println!("cook: {reading:?}");
    let facts = session
        .index()
        .cooked_declarations(&file)
        .expect("the file has a cooked reading");
    println!("facts: {}", facts.len());
    for fact in facts {
        println!(
            "  {} | scope {:?} | kind {:?} | returns {:?}",
            fact.name, fact.scope, fact.kind, fact.returns
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
