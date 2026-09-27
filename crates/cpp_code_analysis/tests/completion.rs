//! Completion's **candidate set**, at the positions a reader actually types at.
//!
//! These are the two questions a user asks of a language server and judges it by — "does it offer the variable in
//! front of me" and "does it offer what the headers declare" — and both were answered with **nothing at all** for
//! one reason: the position query refused a cursor with no name written yet. The candidates were never the problem
//! (the scope chain is walked, innermost first, and the index's visible files are listed after it); the *entry
//! condition* was. So these tests pin the entry condition, with a fixture that has a local, a member and an
//! included header in it.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const HEADER: &str = "int from_the_header(int value);\n";
const SOURCE: &str = "\
#include \"b.h\"
struct Widget { int size; };
int f() {
    int local_variable = 1;
    Widget w;
    return local_variable + w.size;
}
";

fn session_with_the_project() -> Session<MemoryFiles> {
    let memory = MemoryFiles::new()
        .with_file("/p/a.cpp", SOURCE)
        .with_file("/p/b.h", HEADER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/b.h"),
    ]);
    session.index_everything();

    session
}

/// The names offered at an offset, in the order the query returned them.
fn offered_at(offset: usize) -> Option<Vec<String>> {
    let session = session_with_the_project();
    let view = session.view("/p/a.cpp").expect("the file is held");

    match session.name_completions(&view, offset) {
        Known::Yes(found) => Some(
            found
                .names
                .iter()
                .map(|offered| offered.fact.name.clone())
                .collect(),
        ),
        _ => None,
    }
}

/// **A cursor with nothing written yet is where completion lives.** After `return `, the list has to hold the
/// function's own local — that is what go-to-definition already resolves, so the analysis has it — and the names
/// the header contributes, which are the ones a reader cannot see in this file at all.
#[test]
fn a_blank_cursor_offers_the_locals_and_the_included_names() {
    let offset = SOURCE.find("return ").expect("the fixture") + "return ".len();
    let names = offered_at(offset).expect("a blank cursor in a body is answerable");

    assert!(
        names.contains(&"local_variable".to_string()),
        "the local declared above the cursor: {names:?}"
    );
    assert!(
        names.contains(&"w".to_string()),
        "and the other local: {names:?}"
    );
    assert!(
        names.contains(&"from_the_header".to_string()),
        "a name from the included header: {names:?}"
    );
    assert!(
        names.contains(&"Widget".to_string()),
        "and its type: {names:?}"
    );
    assert!(
        names.contains(&"f".to_string()),
        "and the function being written: {names:?}"
    );
}

/// The same list while the name is being typed: the local is still first — the innermost scope has depth 0 — which
/// is what makes the client's own filtering show it at the top.
#[test]
fn a_local_is_offered_while_it_is_being_typed() {
    let offset = SOURCE.find("local_variable + w").expect("the fixture") + "local".len();
    let session = session_with_the_project();
    let view = session.view("/p/a.cpp").expect("the file is held");

    let Known::Yes(found) = session.name_completions(&view, offset) else {
        panic!("a half-written name is answerable");
    };

    assert_eq!(found.prefix, "local");
    let local = found
        .names
        .iter()
        .find(|offered| offered.fact.name == "local_variable")
        .expect("the local is offered");
    assert_eq!(local.depth, 0, "from the scope the cursor is in");
}

/// **A comment or a literal is not code**, and the list there would be a list of names to type into prose.
#[test]
fn a_comment_and_a_literal_offer_nothing() {
    let commented = "int f() { // a note\n    return 0;\n}\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", commented);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");
    let view = session.view("/p/a.cpp").expect("the file is held");

    let inside_the_comment = commented.find("// a").expect("the fixture") + "// a".len();
    assert!(
        !matches!(
            session.name_completions(&view, inside_the_comment),
            Known::Yes(_)
        ),
        "a name in a comment is not code"
    );
}

/// **A member position belongs to the member query.** After `w.` the names in scope are not the answer — `size`
/// is — so the name query declines there, and the handler's dispatch is what picks the right one.
#[test]
fn a_member_position_is_not_a_name_position() {
    let offset = SOURCE.find("w.size").expect("the fixture") + 2;
    let session = session_with_the_project();
    let view = session.view("/p/a.cpp").expect("the file is held");

    assert!(
        !matches!(session.name_completions(&view, offset), Known::Yes(_)),
        "the member query owns this position"
    );

    let Known::Yes(members) = session.member_completions(&view, offset) else {
        panic!("the member query answers it");
    };
    assert!(
        members
            .members
            .members
            .iter()
            .any(|member| member.fact.name == "size"),
        "with the members of the object's type"
    );
}
