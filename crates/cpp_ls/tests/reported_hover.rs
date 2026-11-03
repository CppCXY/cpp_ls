//! **The card a reported hover needed** — a member of a class-template instantiation.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// `Pair<int, int>{1, 2}.sum()` answered "`int` has no member `sum`".
///
/// Two defects met here, and both are fixed: the initialisation's type was the whole spelling as a *name*
/// (`Named { name: "Pair<int, int>" }`, no arguments) rather than the instantiation, so no substitution reached `sum`'s
/// written `T`; and the card read the object's type at its **last byte**, which for a braced initialisation is inside the
/// list — `int`, the first element's type.
///
/// The analysis-layer half is pinned by `cpp_code_analysis`'s
/// `a_braced_initialisation_of_a_class_template_keeps_its_arguments`; this is the card.
#[test]
fn a_member_of_a_class_template_instantiation_is_named() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-template");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "template <typename T, typename U = int>\n\
                  struct Pair {\n\
                      T first;\n\
                      U second;\n\
                      T sum() const { return first + second; }\n\
                  };\n\
                  void f() {\n\
                      int a = Pair<int, int>{1, 2}.sum();\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at =
        source.find("Pair<int, int>{1, 2}.sum").expect("the use") + "Pair<int, int>{1, 2}.".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a member has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        !text.contains("has no member"),
        "the member is resolved through the instantiation: {text}"
    );
    assert!(text.contains("sum"), "the card names the member: {text}");

    let _ = std::fs::remove_dir_all(&root);
}
