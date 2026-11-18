//! **A member named with no qualifier inside its class's own method** — the scope rule that decides it, and the
//! answer a reader gets when it is not followed.
//!
//! Reported from an editor on `CodeFormatServer/src/LSP/LSP.cpp`: in
//! `nlohmann::json lsp::WorkspaceDiagnosticReport::Serialize() { … SerializeArray(items) … }` the hover on `items`
//! listed **three declarations from three classes**, because the name was asked of the index by bare spelling instead
//! of being looked up in the class the definition is written for.

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
        paths.push(std::path::PathBuf::from(path));
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(paths.clone());
    for path in &paths {
        session.load(path);
        session.want_cooked_reading(path);
    }
    session.index_everything();
    session
}

/// **Where the name under the cursor is declared**, as `class::name`, or the refusal as its own text.
fn where_at(session: &Session<MemoryFiles>, path: &str, needle: &str) -> String {
    let path = std::path::Path::new(path);
    let view = session.view(path).expect("the file has a view");
    let at = view
        .written_text()
        .expect("the file is its own text")
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is in the file"));
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the file offset is in the reading");
    match session.definition(&view, ReadingOffset::new(reading.get())) {
        Known::Yes(found) => match &found.fact.scope {
            Some(scope) => format!("{scope}::{}", found.fact.name),
            None => found.fact.name.to_string(),
        },
        Known::Unknown(reason) => format!("unknown: {reason:?}"),
        Known::No => "no".to_string(),
    }
}

const HEADER: &str = "\
struct Widget {
    int size;
    int area();
    int only_the_widget_side;
};

struct Gadget {
    int size;
    int gadget_size;
    int area();
};
";

const WRITTEN: &str = "\
#include \"a.h\"

int Widget::area() {
    return size;
}
";

/// **`size` inside `Widget::area` is `Widget::size`.** No object and no qualifier: the definition's own class is the
/// scope the name is looked up in, and the same spelling in another class is a different declaration.
#[test]
fn a_member_named_without_a_qualifier_is_the_definitions_own_class() {
    let session = session_of(&[("/p/a.h", HEADER), ("/p/a.cpp", WRITTEN)]);
    assert_eq!(
        where_at(&session, "/p/a.cpp", "size"),
        "Widget::size",
        "the name is Widget::size, not the same spelling of another class"
    );
}

/// **A name the class does not declare is not another class's member.** `gadget_size` belongs to `Gadget`; inside
/// `Widget::area` it is not a name this file can use, and the honest answer is that it is not declared — not a jump
/// into a class the reader never mentioned.
#[test]
fn a_name_the_class_does_not_declare_is_not_another_class_s_member() {
    let written = "\
#include \"a.h\"

int Widget::area() {
    return gadget_size;
}
";
    let session = session_of(&[("/p/a.h", HEADER), ("/p/a.cpp", written)]);
    let found = where_at(&session, "/p/a.cpp", "gadget_size");
    assert!(
        !found.contains("Gadget"),
        "another class's member must not answer this name: {found}"
    );
}
