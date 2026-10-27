//! Scratch probe (temporary): what facts does `typedef struct { … } Name;` produce?
use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

#[test]
#[ignore]
fn a_typedef_of_an_anonymous_struct_has_members() {
    let root = std::env::temp_dir().join("cppls-anon-struct");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let source = "\
typedef struct { int a; int b; } Object;
typedef struct Named { int c; } Named;\nstruct Plain { int d; } plain;
int f(void) { Object o; return o.a; }
";
    std::fs::write(root.join("a.c"), source).expect("written");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([PathBuf::from(root.join("a.c"))]);
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(16);
    }

    let summary = session.index().summary(&root.join("a.c")).expect("summarized");
    for fact in &summary.declarations {
        println!(
            "{:?} {:?} scope {:?} type_of {:?}",
            fact.kind, fact.name, fact.scope, fact.type_of
        );
    }

    let view = session.view_of_the_file(&root.join("a.c")).expect("a view");
    // **The scope tree's own names**, which is where a class body's `scope` comes from: a body with no name here is a
    // body whose members cannot be asked about by name.
    for scope in view.scopes.scopes() {
        println!(
            "scope {:?}: {:?} -> {:?}",
            scope.kind,
            scope.name.as_deref(),
            scope
                .bindings
                .iter()
                .map(|binding| binding.name.text())
                .collect::<Vec<_>>()
        );
    }
    for asked in ["Object", "Named"] {
        println!(
            "{asked}: {} posting(s), {} visible; members_of = {:?}",
            session.index().scope_postings(asked), session.index().declarations_in(asked, PathBuf::from(root.join("a.c")).as_path()).len(),
            match session.members_of(&view, asked) {
                Known::Yes(list) => format!("{:?}", list.members.iter().map(|m| m.fact.name.clone()).collect::<Vec<_>>()),
                Known::Unknown(reason) => format!("unknown {reason:?}"),
                Known::No => "no list".to_string(),
            }
        );
    }
}
