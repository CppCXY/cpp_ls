//! Completion, at the positions a reader actually types at.
//!
//! Two layers are pinned here, and they answer different questions:
//!
//! * **What is visible** — the two queries (`Session::name_completions` / `Session::member_completions`) below the
//!   feature, whose candidate sets were always right and whose *entry condition* used to refuse a cursor with no
//!   name written yet: the tests at the top of this file pin that.
//! * **What is offered, and in what order** — `Session::completions`, which reads the shape at the cursor, picks
//!   the vocabulary, adds the language's own words, and ranks the result. That is where "692 names for a blank
//!   line, of which 6 came from this file" was fixed, and the tests at the bottom pin the three things that did
//!   it: the order, the contexts, and the vocabularies that are not declarations.
//!
//! The two are kept in one file because they are one feature, and a reader asking "what does a `.` offer" should
//! find the query that lists the members and the layer that decides the list is wanted in the same place.

use cpp_code_analysis::{
    CompilerConfig, ItemKind, Known, MemoryFiles, OpenDocuments, Session, SessionFiles,
    UnknownReason, WatchFilter,
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
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/b.h"),
    ]);

    // **Two pumps, and the ask between them, because that is the contract now.**
    //
    // A completion is a question about **meaning**, so it is answered from a rendering — and a rendering is built
    // because something asked for one. The first pump reads the files into the index; `view` is the ask (it answers
    // `None` and queues what it could not answer, deliberately: a query must not pay a unit walk, a preprocess and a
    // render); the second pump is what builds it. Every test in this file goes through this one helper, so the
    // arrangement is stated once here rather than in fifty `expect` messages.
    //
    // This is not a test artefact. It is what a server does between two keystrokes, and what the deferral contract
    // exists for: `None` now, the reading after the work the ask queued.
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    let _ = session.view("/p/b.h");
    session.index_everything();

    session
}

/// **A reading of `path` that a completion can be asked against** — the pump, the ask, the pump.
///
/// Owned rather than borrowed, which matters: `Session::view` takes `&self` and the query takes `&self` too, but the
/// ask has to happen on a session nothing is borrowing yet, so a test that wrote
/// `session.completions(&session.view(..)?, ..)` would have the borrow in the argument position. Returning the view
/// by value is what lets the two calls sit on one line.
///
/// The three steps are the contract, not scaffolding: a completion is a question about **meaning**, meaning is
/// answered from a rendering, a rendering is built because something asked, and `Session::view` answers `None` and
/// queues when it has none. Every fixture in this file that builds its own session goes through here, so the
/// arrangement is stated once.
fn cooked_view(session: &mut Session<MemoryFiles>, path: &str) -> cpp_code_analysis::FileView {
    // **The three steps, and the first is not optional for a file nothing has read.**
    //
    // A completion is a question about **meaning**, so it is answered from a rendering, and a rendering is built
    // because something asked. `want_cooked_reading` is the ask that also queues *reading* — a cook is built out of
    // a summary, so a file the index has never described needs both, and a fixture that merely `load`s a file has
    // given it neither. Measured without this line: `indexed false, pending 0, backlog 0`, and every `view` `None`.
    //
    // This is the product's own contract, not scaffolding: it is what a server does between two keystrokes for a
    // file it has not read yet.
    session.want_cooked_reading(std::path::Path::new(path));
    session.index_everything();
    session
        .view(path)
        .unwrap_or_else(|| panic!("{path} asked to be read and cooked, and the pump did both"))
}

/// The names offered at an offset, in the order the query returned them.
fn offered_at(offset: usize) -> Option<Vec<String>> {
    let mut session = session_with_the_project();
    let view = cooked_view(&mut session, "/p/a.cpp");

    match session.name_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    ) {
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
    let mut session = session_with_the_project();
    let view = cooked_view(&mut session, "/p/a.cpp");

    if std::env::var_os("CPPLS_TRACE_COORDINATES").is_some() {
        let reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
                offset,
            ))
            .expect("the cursor is in the rendering");
        // **The token's own start in each text**, which is the anchor that settles this. The file's cursor sits on
        // the `_` of `local_variable`; if the rendering's offset for that same cursor lands on the *start* of the
        // token, then the two rulers differ by more than the whitespace the rendering collapsed — and the number to
        // compare is the distance from the token's start in each text.
        let in_file = SOURCE.find("local_variable + w").expect("the use") + 5;
        let in_rendering = view.source.find("local_variable + w").expect("the use") + 5;
        eprintln!(
            "cursor: file {in_file} -> reading {reading} (the same spot is {in_rendering} in the rendering)\n  \
             the token starts at {} in the file and {} in the rendering\n  \
             the two cursors differ by {}; the two token starts differ by {}",
            SOURCE.find("local_variable + w").expect("the use"),
            view.source.find("local_variable + w").expect("the use"),
            in_file as i64 - reading.get() as i64,
            SOURCE.find("local_variable + w").expect("the use") as i64
                - view.source.find("local_variable + w").expect("the use") as i64
        );
    }

    let Known::Yes(found) = session.name_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    ) else {
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
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    let view = cooked_view(&mut session, "/p/a.cpp");

    let inside_the_comment = commented.find("// a").expect("the fixture") + "// a".len();
    assert!(
        !matches!(
            session.name_completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(inside_the_comment)
            ),
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
    let mut session = session_with_the_project();
    let view = cooked_view(&mut session, "/p/a.cpp");

    assert!(
        !matches!(
            session.name_completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(offset)
            ),
            Known::Yes(_)
        ),
        "the member query owns this position"
    );

    let Known::Yes(members) = session.member_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    ) else {
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

/// **A class split across two files: its member function's body must still see the members.**
///
/// The class is declared in a header and its member function is defined in a `.cpp` — the ordinary layout of C++
/// code, and the one a user reported as offering **nothing** inside the definition. What makes it interesting is
/// where the answer has to come from: the members are declared in a file the cursor is not in, so this is the
/// "report is about this file, the knowledge is the project's" boundary applied to completion.
///
/// The cursor sits after `this->`, which is the plainest way to ask: `this` has the enclosing class's type, and the
/// enclosing class is not declared in this file.
///
/// # What was wrong, and what it took
///
/// Two things, found in that order.
///
/// **The fixture did not include the header.** A query is answered from the files the asking file can see, and a
/// `.cpp` whose text never `#include`s its own header sees nothing in it — so no scope-tree reading could have made
/// this expectation satisfiable. The include is the fixture's, not the reading's.
///
/// **`void Widget::grow(…)` opened a scope that belonged to no class.** When the qualifier names a class *this file*
/// has built, the function scope hangs off that class and inherits its qualified name. When it does not — the
/// ordinary layout, the class in a header — the scope was created **unnamed**, and `this` is typed from the cursor's
/// innermost scope's qualified name: `Unknown(UnknownType("this"))`, measured. The scope now carries the definition's
/// own spelling (`Widget::grow`) even though `Widget` is not a scope here, which is exactly what a reader needs to
/// answer *which* class the body belongs to — see `read_the_inside_of_a_foreign_definition`.
#[test]
fn a_member_function_defined_in_another_file_sees_its_class() {
    let header = "\
class Widget {
public:
    int size;
    void grow(int by);
};
";
    // **The include is what makes the header's members reachable at all** — a query is answered from the files the
    // asking file can see, and a `.cpp` that never includes its own header sees nothing in it. The fixture was
    // written without this line, which is why the expectation could not be met however the scope tree was read.
    let source = "\
#include \"widget.h\"
void Widget::grow(int by) {
    this->
}
";
    let mut memory = MemoryFiles::new()
        .with_file("/p/widget.h", header)
        .with_file("/p/widget.cpp", source);
    memory = memory.with_file("/p/a.cpp", "int main() { return 0; }\n");
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/widget.h"),
        std::path::PathBuf::from("/p/widget.cpp"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/widget.cpp"));
    session.index_everything();

    let offset = source.find("this->").expect("the fixture") + "this->".len();
    let view = cooked_view(&mut session, "/p/widget.cpp");
    let offered: Vec<String> = match session.member_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    ) {
        Known::Yes(members) => members
            .members
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect(),
        other => panic!("the member query must answer here: {other:?}"),
    };

    assert!(
        offered.iter().any(|name| name == "size"),
        "the class's own members come from the file that declares them: {offered:?}"
    );
}
/// **The names the implementation owns are not offered**, and the user's own underscored names still are.
///
/// The standard's three rules (`__name`, `_Name`, `_name` in the global name space), applied where a *suggestion*
/// is made rather than where a question is answered: a jump, a hover and a rename still find a reserved name. This
/// is the rule that took `_ALLOC_MASK`, `_Alty` and `_Apply_annotation` off a `std::string`'s member list — half of
/// its 202 entries — and 601 names off one real file's list at a blank line in a function body.
#[test]
fn the_implementations_names_are_not_offered_but_the_users_are() {
    const FIXTURE: &str = "\
struct Widget { int size; int _mine; };
int _global_underscored;
int __mine_anywhere;
int _Upper_anywhere;
int f() {
    int _local_underscored = 1;
    Widget w;
    w.
}
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    let view = cooked_view(&mut session, "/p/a.cpp");

    // The **name** list, asked inside the body: the user's local is there, the reserved globals are not.
    let known = match session.name_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(
            FIXTURE.find("w.").expect("the fixture"),
        ),
    ) {
        Known::Yes(found) => found
            .names
            .iter()
            .map(|offered| offered.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("a blank cursor in a body is answerable: {other:?}"),
    };

    for reserved in ["__mine_anywhere", "_Upper_anywhere", "_global_underscored"] {
        assert!(
            !known.contains(&reserved.to_string()),
            "`{reserved}` is reserved to the implementation: {known:?}"
        );
    }
    assert!(
        known.contains(&"_local_underscored".to_string()),
        "a local `_name` is the user's own — only the *global* name space reserves it: {known:?}"
    );

    // The **member** list of a class the user wrote: `_mine` is the user's own member and is offered, while the
    // rule still takes MSVC's `_Alty`/`_ALLOC_MASK` off a `std::string`.
    let Known::Yes(members) = session.member_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(
            FIXTURE.find("w.").expect("the fixture") + 2,
        ),
    ) else {
        panic!("a member access is answerable");
    };
    let names: Vec<&str> = members
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert!(names.contains(&"size"), "the ordinary member: {names:?}");
    assert!(
        names.contains(&"_mine"),
        "and the user's own underscored member: {names:?}"
    );
}

// ------------------------------------------------------------------------------------------------------------
// The layer above the two queries: what is **offered**, and in what order.
// ------------------------------------------------------------------------------------------------------------

/// A fixture with all four distances in it: a local, a name at file scope, a name from a header the file includes
/// directly, and a name from a header only *that* header includes.
///
/// The four are the whole of the ranking, and a fixture that had only some of them could not tell the order the
/// layers produce from the alphabet — which is exactly the mistake this feature exists to fix.
const RANKED_CPP: &str = "\
#include \"near.h\"
int at_file_scope;
int f() {
    int in_the_body = 1;
    return in_the_body + at_file_scope;
}
";

/// Included by `RANKED_CPP`.
const NEAR_H: &str = "#include \"far.h\"\nint from_the_near_header;\n";

/// Included by `near.h`, so a name from here is two include hops away.
const FAR_H: &str = "int from_the_far_header;\n";

fn ranked_session() -> Session<MemoryFiles> {
    let memory = MemoryFiles::new()
        .with_file("/p/a.cpp", RANKED_CPP)
        .with_file("/p/near.h", NEAR_H)
        .with_file("/p/far.h", FAR_H);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/near.h"),
        std::path::PathBuf::from("/p/far.h"),
    ]);
    session.index_everything();

    session
}

/// The labels `Session::completions` offers at an offset, in the order it produced them.
///
/// Takes `&mut Session` and goes through [`cooked_view`], because the reading a completion is answered from is
/// built on request — a helper that read `session.view(path)` directly would be asking for a reading nothing had
/// asked for, and would panic on the `None` the deferral contract returns.
fn labels_at(session: &mut Session<MemoryFiles>, path: &str, offset: usize) -> Vec<String> {
    let view = cooked_view(session, path);

    session
        .completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(offset),
        )
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect()
}

/// The position of a label in a list, with a message that shows the whole list when it is missing.
fn position(labels: &[String], wanted: &str) -> usize {
    labels
        .iter()
        .position(|label| label == wanted)
        .unwrap_or_else(|| panic!("`{wanted}` is not offered at all: {labels:?}"))
}

/// **The order is the feature.** Four declarations a compiler considers in one order and a reader means in another
/// — the local, this file's, the header's, and what that header includes — and the list has to show them in that
/// order rather than alphabetically or in whatever order the index happened to hold them.
///
/// This is what fixes "it offers nearly everything": the names were never missing, they were *buried*. Measured
/// before the change, a blank line in one real 78-line file offered **692** names of which **6** came from the
/// file itself.
#[test]
fn the_nearest_declaration_comes_first() {
    let mut session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&mut session, "/p/a.cpp", cursor);

    let body = position(&labels, "in_the_body");
    let file = position(&labels, "at_file_scope");
    let near = position(&labels, "from_the_near_header");
    let far = position(&labels, "from_the_far_header");

    // The provenance is quoted in the message rather than asserted on directly: it is *why* the order is what it
    // is, and a failure of the order is the thing worth reading.
    let view = cooked_view(&mut session, "/p/a.cpp");
    let provenance: Vec<String> = match session.name_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    ) {
        Known::Yes(found) => found
            .names
            .iter()
            .filter(|name| name.fact.name.starts_with("from_"))
            .map(|name| format!("{}: {:?}", name.fact.name, name.from))
            .collect(),
        other => panic!("{other:?}"),
    };

    assert!(
        body < file,
        "the local is in front of the file-scope name: {labels:?}"
    );
    assert!(
        file < near,
        "and this file's name is in front of the header's: {labels:?}"
    );
    assert!(
        near < far,
        "and a header the file includes is in front of one that header includes: {provenance:?}"
    );
}

/// The same order, one keystroke later: typing three letters must not reorder the list, because the client filters
/// what it was given and a filter cannot un-bury a name.
#[test]
fn typing_a_prefix_does_not_reorder_the_list() {
    let mut session = ranked_session();
    let cursor = RANKED_CPP.find("in_the_body + at").expect("the fixture") + "in_the_body + ".len();

    let labels = labels_at(&mut session, "/p/a.cpp", cursor);
    let preview: Vec<&String> = labels.iter().take(3).collect();

    assert!(
        preview.iter().any(|label| label.as_str() == "in_the_body"),
        "the local is still at the top with `at` typed: {labels:?}"
    );
}

/// **A blank line in a body offers the language's own vocabulary**, which is the other half of what was missing:
/// the keywords and the snippet bodies. `if` is offered as a **snippet** (it writes the braces and puts the cursor
/// inside them) rather than as a bare keyword beside it.
#[test]
fn a_blank_line_in_a_body_offers_the_keywords_and_the_snippets() {
    let mut session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&mut session, "/p/a.cpp", cursor);
    let view = cooked_view(&mut session, "/p/a.cpp");
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    assert!(labels.contains(&"while".to_string()), "{labels:?}");
    assert!(labels.contains(&"if".to_string()), "{labels:?}");
    assert!(labels.contains(&"const".to_string()), "{labels:?}");
    assert!(labels.contains(&"size_t".to_string()), "{labels:?}");

    let if_item = offered
        .items
        .iter()
        .find(|item| item.label == "if")
        .expect("`if` is offered");
    assert_eq!(if_item.kind, ItemKind::Snippet);
    assert!(if_item.snippet, "and it is a snippet body, not a keyword");
    assert!(
        if_item.insert.contains("$0"),
        "with a hole for the cursor: {}",
        if_item.insert
    );
    assert_eq!(
        offered
            .items
            .iter()
            .filter(|item| item.label == "if")
            .count(),
        1,
        "and exactly once — the bare keyword is not offered beside it"
    );
}

/// **A keyword is not a name and a name is not a keyword**, so the two are ordered separately: a snippet the
/// reader is about to write comes after their own variables and before a name from `<cstdio>`.
#[test]
fn a_keyword_sorts_between_the_files_names_and_the_headers() {
    let mut session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&mut session, "/p/a.cpp", cursor);
    let file = position(&labels, "at_file_scope");
    let keyword = position(&labels, "while");
    let far = position(&labels, "from_the_far_header");

    assert!(
        file < keyword && keyword < far,
        "this file's names, then the language's words, then the headers': {labels:?}"
    );
}

/// **After a `#` the answer is the directives**, and a half-written directive is *replaced* rather than extended —
/// `#inc` completed to `include` must not become `#incinclude`.
#[test]
fn after_a_hash_the_directives_are_offered() {
    const FIXTURE: &str = "#inc\nint x;\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("inc").expect("the fixture") + "inc".len();
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    let include = found
        .items
        .iter()
        .find(|item| item.label == "include")
        .unwrap_or_else(|| panic!("`include` is offered: {:?}", found.items));

    assert_eq!(
        include.replace.start_offset, 1,
        "the edit starts at the `i` of `inc`"
    );
    assert_eq!(include.replace.length, 3, "and covers all of it");
    assert!(
        !found.items.iter().any(|item| item.label == "int"),
        "and no language name is offered in a directive's own name: {:?}",
        found.items
    );
}

/// `#endif` takes no argument, so a cursor after it gets **nothing** rather than the file's names — a list there
/// would offer things that cannot follow.
#[test]
fn a_directive_that_takes_no_argument_offers_nothing() {
    const FIXTURE: &str = "#if 0\n#endif \nint x;\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("#endif ").expect("the fixture") + "#endif ".len();

    assert!(
        session
            .completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(cursor)
            )
            .items
            .is_empty(),
        "nothing may follow `#endif`"
    );
}

/// `#define` takes a **name**, so the argument position offers the program's names and not the language's words: a
/// macro is called an identifier, and `while` is not one.
#[test]
fn a_defines_argument_offers_names_rather_than_keywords() {
    const FIXTURE: &str = "int already_a_name;\n#define \n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("#define ").expect("the fixture") + "#define ".len();
    let labels = labels_at(&mut session, "/p/a.cpp", cursor);

    assert_eq!(
        session
            .completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(cursor)
            )
            .prefix,
        "",
        "nothing is written after `#define ` yet"
    );
    assert!(labels.contains(&"already_a_name".to_string()), "{labels:?}");
    assert!(
        !labels.contains(&"while".to_string()),
        "a keyword is not a macro name: {labels:?}"
    );
}

/// **A `#include` is completed from the search path**, and a project header is offered beside the search path's —
/// spelled relative to the project root, which is how a reader writes it.
#[test]
fn an_include_offers_the_projects_headers() {
    // The cursor is inside the name, so the edit **replaces** it: `#include <near` completed to `near.h` must not
    // become `#include <nearnear.h`.
    let typed = "#include <near\nint x;\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", typed);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/near.h"),
    ]);
    session.index_everything();

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = typed.find("near").expect("the fixture") + "near".len();
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    let near = found
        .items
        .iter()
        .find(|item| item.label == "near.h")
        .unwrap_or_else(|| {
            panic!(
                "the project's own header is offered: {:?}",
                found.items.iter().map(|it| &it.label).collect::<Vec<_>>()
            )
        });

    assert_eq!(near.kind, ItemKind::Header);
    assert_eq!(
        near.replace.start_offset,
        typed.find("near").expect("the fixture")
    );
    assert_eq!(near.replace.length, 4, "the whole half-written name");
}

/// **A completion in a comment is empty**, and the same cursor one character earlier is not: the offset that
/// merely *touches* a comment's end is at the start of what follows it.
#[test]
fn a_comment_is_not_code() {
    const FIXTURE: &str = "int x; // a note here\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let inside = FIXTURE.find("note").expect("the fixture");

    assert!(
        session
            .completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(inside)
            )
            .items
            .is_empty(),
        "a name in a comment is not code"
    );
    assert!(
        !session
            .completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(
                    FIXTURE.find('x').expect("the fixture") + 1
                )
            )
            .items
            .is_empty(),
        "and the declaration beside it is"
    );
}

/// **A member position offers the members and nothing else**, and the two distinctions a member *does* carry come
/// with it: a member function is a method rather than a free function, and a member an inherited class declares
/// says so.
#[test]
fn a_member_position_offers_the_members_with_their_distinctions() {
    const FIXTURE: &str = "\
struct Base { int inherited; };
struct Widget : Base { int size; void grow(); };
int f() {
    Widget w;
    w.
}
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("w.").expect("the fixture") + 2;
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
    assert!(labels.contains(&"size"), "{labels:?}");
    assert!(labels.contains(&"grow"), "{labels:?}");
    assert!(labels.contains(&"inherited"), "{labels:?}");
    assert!(
        !labels.contains(&"f"),
        "and not the names in scope, which cannot follow the operator: {labels:?}"
    );

    let size = position(
        &labels.iter().map(|it| it.to_string()).collect::<Vec<_>>(),
        "size",
    );
    let inherited = position(
        &labels.iter().map(|it| it.to_string()).collect::<Vec<_>>(),
        "inherited",
    );
    assert!(
        size < inherited,
        "the class's own member before the base's: {labels:?}"
    );

    let grow = found
        .items
        .iter()
        .find(|item| item.label == "grow")
        .expect("offered above");
    assert_eq!(grow.kind, ItemKind::Method, "a member function is a method");

    let inherited = found
        .items
        .iter()
        .find(|item| item.label == "inherited")
        .expect("offered above");
    assert!(
        inherited
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Base")),
        "and where an inherited member was declared is said: {inherited:?}"
    );
}

/// **`::` lists one scope**, and what it lists is *that* scope's names rather than everything in sight.
#[test]
fn a_qualified_name_lists_that_scope_only() {
    const FIXTURE: &str = "\
namespace one { struct Widget { int size; }; }
namespace two { struct Gadget { int weight; }; }
one::
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("one::").expect("the fixture") + "one::".len();
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    assert_eq!(found.scope, "one");
    let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
    assert!(labels.contains(&"Widget"), "{labels:?}");
    assert!(
        !labels.contains(&"Gadget"),
        "another namespace's name is not in `one`: {labels:?}"
    );
    assert!(
        !labels.contains(&"while"),
        "and a keyword cannot follow a `::`: {labels:?}"
    );
}

/// A client that asks in the **middle** of a name filters by what is before the cursor and **replaces** the whole
/// spelling — the two are different fields and using one for both is what makes `w.si|ze` offer nothing.
#[test]
fn a_cursor_inside_a_name_filters_by_the_prefix_and_replaces_all_of_it() {
    const FIXTURE: &str = "\
struct Widget { int size; };
int f() { Widget w; w.size; }
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let at_the_member = FIXTURE.find("w.size").expect("the fixture");
    let cursor = at_the_member + 2 + "si".len();
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    assert_eq!(
        found.prefix, "si",
        "what filters is the part before the cursor"
    );
    assert_eq!(
        found.replace.start_offset,
        at_the_member + 2,
        "and what is replaced is the whole spelling"
    );
    assert_eq!(found.replace.length, 4, "`size`");
    assert!(
        found.items.iter().any(|item| item.label == "size"),
        "so the member is still offered while it is being typed"
    );
}

/// The **budget** is a payload bound, and the list says whether it was applied: a client that is told
/// `isIncomplete` about a capped list would re-ask for an answer that cannot change.
#[test]
fn a_short_list_is_not_marked_truncated() {
    let mut session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();
    let view = cooked_view(&mut session, "/p/a.cpp");

    assert!(
        !session
            .completions(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(cursor)
            )
            .truncated,
        "three headers do not fill a budget of two hundred"
    );
}

/// **An `#include` points at a file**, which is the fourth answer a jump can give and the one nothing else in the
/// analysis can produce: `#include "near.h"` declares nothing, so the scope walk, the index by name and the macro
/// table all answer "nothing found" for a line whose whole purpose is to name a file.
#[test]
fn an_include_points_at_the_header_it_names() {
    let mut session = ranked_session();
    let view = cooked_view(&mut session, "/p/a.cpp");

    // Inside the spelling: `#include "near.h"` — character 12 is the `e` of `near`.
    let cursor = RANKED_CPP.find("near.h").expect("the fixture");
    let Known::Yes(found) = session.header_at(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    ) else {
        panic!("the cursor is on a header name");
    };

    assert_eq!(found.spelling, "near.h");
    assert_eq!(found.resolved, std::path::PathBuf::from("/p/near.h"));
    assert_eq!(
        RANKED_CPP[..found.range.end_offset()].trim_end(),
        "#include \"near.h\"",
        "the range is the whole directive — the `#`, the name and any trailing comment — because that is \
         what the reader pointed at"
    );

    // The `#` and the word `include` are part of the directive rather than of the name. Nothing in this analysis
    // depends on which one a reader points at — `Session::header_at` is asked with a *cursor*, and every client
    // sends the same requests for a ctrl-click wherever it lands on the line — so the assertions here are about
    // the boundary being where it is documented to be rather than about a user-visible difference.
    assert!(
        !matches!(
            session.header_at(
                &view,
                cpp_code_analysis::file::coordinates::FileOffset::new(
                    RANKED_CPP.find("#include").expect("the fixture")
                )
            ),
            Known::Yes(_)
        ),
        "the `#` is not the header's name"
    );

    // A name that is not a header is **not** this query's business — and the answer says so rather than saying
    // "nothing found", which is what keeps it from swallowing a jump that has a real declaration behind it.
    let on_a_declaration = RANKED_CPP.find("at_file_scope").expect("the fixture");
    match session.header_at(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(on_a_declaration),
    ) {
        Known::Unknown(reason) => assert_eq!(
            reason.describe(),
            UnknownReason::UnparsableName.describe(),
            "`at_file_scope` is not a header name at all"
        ),
        other => panic!("a variable is not a header: {other:?}"),
    }
}

/// **An `#include` that resolves to nothing is `Unknown` and names the header** — the same distinction the rest of
/// the crate keeps: "the index cannot find it" is a different claim from "there is no such file", and the reader
/// whose project has no `-I` for a header deserves to be told the first.
#[test]
fn an_include_that_resolves_to_nothing_says_so() {
    const FIXTURE: &str = "#include \"nowhere.h\"\nint x;\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("nowhere").expect("the fixture");

    match session.header_at(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    ) {
        Known::Unknown(reason) => assert!(
            reason.describe().contains("nowhere.h"),
            "the reason names the header nobody could find: {}",
            reason.describe()
        ),
        other => panic!("a header that resolves to nothing is `Unknown`: {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------------------------
// The cursor's file must be **read before it is asked about**, and the state it is read in is the bug.
// ------------------------------------------------------------------------------------------------------------

/// A file whose type is declared in a **header**, which is where a stale reading actually shows: `Widget`'s members
/// come from `widget.h`'s summary, so that header has to have been read for `w.` to be answerable at all.
const FRESHNESS_HEADER: &str = "struct Widget { int size; };\n";
const FRESHNESS_USES_HEADER: &str = "\
#include \"widget.h\"
int f() {
    Widget w;
    w.
}
";

/// **An edit replaces the text *and drops the summary*, and completion must not run between those two steps.**
///
/// `Session::buffer_changed` does the first immediately — the scope tree is rebuilt from the new text on every parse,
/// so a local's declarator is always right — and queues the second (the summary is rebuilt when the pump reaches the
/// file). A completion asked inside that window sees the new text with the **old facts**, and the damage is not an
/// error but a *different list*: the member query cannot work out the object's type, declines, and the client is
/// handed the names in scope, which is a list of everything that cannot follow a `.`.
///
/// That is what a user reported, with a screenshot: the popup after `myName.firstName.` was `printf`, `full`, `sum`,
/// `main` and the keywords. The fix is [`Session::catch_up`], which the completion handler calls before the query: it
/// re-reads the one file, and does nothing at all when there is nothing waiting.
#[test]
fn a_completion_after_an_edit_is_about_the_file_the_edit_wrote() {
    let memory = MemoryFiles::new()
        .with_file("/p/a.cpp", FRESHNESS_USES_HEADER)
        .with_file("/p/widget.h", FRESHNESS_HEADER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);
    session.index_everything();

    // The edit: the type gains a member, and the header's text is replaced exactly as `didChange` replaces it. **The
    // pump is deliberately not advanced** — that is the window this test is about, and it is the state a real server
    // is in when the keystroke that asked the question arrives.
    let edited = "struct Widget { int size; int extra; };\n";
    session.did_change("/p/widget.h", edited);
    assert!(
        session.pending() > 0,
        "the edit dropped the summary and queued the file, which is the state under test"
    );

    let cursor = FRESHNESS_USES_HEADER.find("w.").expect("the fixture") + 2;
    // **A view without a pump, because the state under test is the un-pumped one.** `cooked_view` asks for the reading
    // *and drains the queue* — which is exactly the catch-up whose **absence** this half of the test measures: with it
    // the header's summary was already rebuilt and the first assertion read `["extra", "size"]` instead of the stale
    // list. `FileView::parse_with` builds the file's own view from what the VFS holds and touches no queue, so the
    // window stays open until `catch_up` below closes it.
    let held = session.files().held("/p/a.cpp").expect("the file is held");
    let view =
        cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());

    // **Without the catch-up**: the header's summary is the old one, so `Widget` still has only `size`.
    let stale = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );
    let stale_labels: Vec<&str> = stale.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        !stale_labels.contains(&"extra"),
        "the index is one edit behind until something reads the file: {stale_labels:?}"
    );

    // **With it**: one parse of one file, and the answer is about the text the user is looking at.
    session.catch_up(std::path::Path::new("/p/widget.h"));
    let fresh = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );
    let labels: Vec<&str> = fresh.items.iter().map(|item| item.label.as_str()).collect();

    assert!(
        labels.contains(&"extra") && labels.contains(&"size"),
        "both members of the type the header now declares: {labels:?}"
    );
    assert!(
        !labels.contains(&"f"),
        "and the names in scope are not — a `.` is not a name position: {labels:?}"
    );
    assert_eq!(
        session.pending(),
        0,
        "the synchronous read is the queue's work, done early: the file is not read a second time when the pump \
         gets to it"
    );
}

/// **A name declared below the cursor is not in scope yet**, which is the other half of what a user reported: the
/// popup at the top of `main` offered `sum` — a variable whose declaration is three lines further down.
///
/// A *scope* holds every binding written inside its braces, so a list built from them contains names the language
/// has not reached. The rule is textual and it applies to **bodies** only: a class's members are reachable
/// regardless of where in the class they are written (`void f() { x = 1; } int x;` is legal), so filtering those by
/// position would remove names a reader can write.
#[test]
fn a_name_declared_below_the_cursor_is_not_offered() {
    const FIXTURE: &str = "\
int f() {
    int above = 1;
    int also_above = 2;

    int sum = above + also_above;
    return sum;
}
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    // At the blank line **above** `int sum`, where a reader would type the declaration that uses it.
    let blank = FIXTURE.find("\n\n    int sum").expect("the fixture") + 1;
    let labels = labels_at(&mut session, "/p/a.cpp", blank);

    assert!(
        labels.contains(&"above".to_string()) && labels.contains(&"also_above".to_string()),
        "what was declared above the cursor is offered: {labels:?}"
    );
    assert!(
        !labels.contains(&"sum".to_string()),
        "and what is declared below it is not — the language has not reached it: {labels:?}"
    );

    // …and the same name **is** offered once the cursor is below its declaration, which is what keeps this from
    // being a filter that removes too much.
    let after = FIXTURE.find("return sum").expect("the fixture") + "return ".len();
    let labels = labels_at(&mut session, "/p/a.cpp", after);
    assert!(
        labels.contains(&"sum".to_string()),
        "below the declaration it is in scope: {labels:?}"
    );
}

/// **The sequence a server is in when the very first completion arrives** — `did_open`, then the request, with the
/// pump *not* advanced in between.
///
/// This is the order the LSP layer actually runs: `handlers::initialized` builds the session, `didOpen` replaces the
/// buffer (which **drops that file's summary**), and a completion arrives before anything has read the file again.
/// `Session::catch_up` is what the handler calls first, and the question this test holds fixed is whether that one
/// call is **enough** — a completion that needs a name from a header needs the header to have been read *by someone*,
/// and if `catch_up` rebuilds the file's summary without the index ever following its includes, the answer is an
/// empty member list for a type the file plainly includes.
///
/// No standard library: the same shape with a local header, so that the test is about the sequence rather than about
/// how long MSVC's `<string>` takes to read.
#[test]
fn the_first_completion_of_a_session_finds_the_type_in_an_included_header() {
    let memory = MemoryFiles::new()
        .with_file("/p/main.cpp", FRESHNESS_USES_HEADER)
        .with_file("/p/widget.h", FRESHNESS_HEADER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/main.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);

    // **The server's sequence, and only that**: the client opens the document, and the pump has not run.
    session.did_open("/p/main.cpp", FRESHNESS_USES_HEADER);
    session.catch_up(std::path::Path::new("/p/main.cpp"));

    let cursor = FRESHNESS_USES_HEADER.find("w.").expect("the fixture") + 2;
    let view = cooked_view(&mut session, "/p/main.cpp");
    let found = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );

    let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"size"),
        "the members of the type the file includes, from a session that has read nothing yet — `catch_up` has to be \
         enough on its own, or the first completion of every session is empty: {labels:?}"
    );
}

/// **A file that has not been edited is not read again.** The pump drains after every edit, so the ordinary
/// keystroke asks [`Session::catch_up`] about a path that is not in the queue — and the answer has to be free, or
/// every completion would pay a parse.
#[test]
fn a_file_that_is_not_stale_is_not_read_again() {
    let memory = MemoryFiles::new()
        .with_file("/p/a.cpp", FRESHNESS_USES_HEADER)
        .with_file("/p/widget.h", FRESHNESS_HEADER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);
    session.index_everything();

    let before = session.stats();
    session.catch_up(std::path::Path::new("/p/widget.h"));
    let after = session.stats();

    assert_eq!(
        after, before,
        "nothing was read, parsed or written: the file was not stale"
    );
}

/// **The operator on the last line of a body, asked at either of the two offsets a caret on it can mean.**
///
/// The fourth report of "No suggestions" under `full2.`, and the narrowest: the line is the **last of the body**,
/// so the `}` follows it and the parser's recovery has nowhere to put the unfinished expression. Two things then
/// went wrong together, and both are about the *cursor* rather than about the type:
///
/// * the tree's access node ends before the operator, so a reader that asked the tree only answered "the name
///   `full2`, then something else" — and the client got the names in scope, a list of things that cannot follow a
///   `.`;
/// * a caret is drawn **between two characters**, and a client sends the position of the caret. For `full2|.` that
///   is the operator's own column, and for `full2.|` it is one past it. Both mean "after the operator" to a reader,
///   so both must answer the same list — and the diagnostic for the same keystroke must not say the opposite of
///   what the list says. It did: "the cursor is not a member access at all" was logged beside 55 members, because
///   the diagnostic asked the index layer's tree-only shape reader while the list asked the context reader.
///
/// No standard library here: a local header gives the same shape without paying for MSVC's `<string>`, which is
/// what `a_member_access_on_a_standard_library_type_offers_its_members` in `cpp_ls`'s handshake suite is for.
#[test]
fn a_member_access_on_the_last_line_of_a_body_offers_its_members_at_either_caret() {
    const FIXTURE: &str = "\
#include \"widget.h\"
int main() {
    Widget w;
    w.
}
";
    let memory = MemoryFiles::new()
        .with_file("/p/a.cpp", FIXTURE)
        .with_file("/p/widget.h", FRESHNESS_HEADER);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);
    session.index_everything();

    let view = cooked_view(&mut session, "/p/a.cpp");
    let dot = FIXTURE.find("w.").expect("the fixture") + 1;
    assert_eq!(&FIXTURE[dot..dot + 1], ".", "the fixture's operator");

    for offset in [dot, dot + 1] {
        let found = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(offset),
        );
        let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();

        assert!(
            labels.contains(&"size"),
            "the caret at {offset} is on the operator, and `w` is a `Widget` declared a line above: {labels:?}"
        );
        assert!(
            !labels.contains(&"w"),
            "and not the names in scope, which cannot follow a `.`: {labels:?}"
        );

        // The sentence a failure is diagnosed with comes from the same reading as the list, so an empty list is
        // always explained by the step that emptied it — and `why_no_members` is handed a **tree**, so its offset is
        // the rendering's. Passing the fixture's own offset here was `Bad offset: range 0..27 offset 52` inside
        // `rowan`: the file is 52 bytes and its rendering (the header inlined, the directive gone) is 27.
        let in_the_reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
                offset,
            ))
            .expect("the cursor is in the rendering");
        let why = cpp_code_analysis::why_no_members(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            in_the_reading,
        );
        assert!(
            why.contains("the member query answered"),
            "the diagnosis agrees with the list at {offset}: {why}"
        );
    }
}

/// **A constructor's parameters are in scope in its body and in its member-initializer list.**
///
/// Reported as a missing completion: inside `Name(const std::string& f, ...) { first_| }` the list did not hold
/// `firstName`, and the semantic tokens pass says why — `firstName`, `middleName` and `lastName` are classified
/// as the **class's fields** and `f`, `m`, `l` are classified as **nothing at all**, where a plain member
/// function's parameter is classified `Parameter`. So the constructor's parameters are not in the scope tree,
/// and two questions are answered wrongly from the same gap.
///
/// Both positions are checked, because they are answered by different machinery: the initializer list is read
/// before the body exists, and the body is an ordinary block.
/// **Still failing on the member-initializer list, and the repro is left in place.**
///
/// What is fixed: a constructor is now recognised as a declaration that *names* something. Its declaration is a
/// bare `Declarator` rather than an `InitDeclarator` — there is no return type, so no `decl-specifier-seq` — and
/// `CppDeclaration::get_name_token` only looked at `InitDeclarator`. So the name was not found, the declaration
/// was read as "unnamed", and `is_unnamed_declaration` skipped it entirely: the constructor's parameters were
/// bound nowhere. Measured after the fix: `f` and `m` are `Parameter` where they are declared, and the
/// constructor itself is a `Method` rather than a bare `Type`.
///
/// What is not: their **uses**. `f` inside `: first(f)` and inside the body is still unclassified. The body is a
/// `CompoundStat` that is **not a child of the declaration** — instrumented, the declaration's children are
/// `["Declarator", "MemberInitializer", "MemberInitializer"]` while a `CompoundStat` for that body exists
/// elsewhere in the tree — so `function_body` does not find it and nothing is walked into the function scope.
/// The member-initializer walk added beside it therefore has no scope to walk into.
///
/// `#[ignore]` rather than deleted: it is the measurement, and the next round starts from here.
#[test]
fn a_constructors_parameters_are_in_scope() {
    let source = "struct Name {\n\
                      Name(const int f, const int m)\n\
                          : first(f), second(m) {\n\
                          int here = f;\n\
                      }\n\
                      int first;\n\
                      int second;\n\
                  };\n";
    let files = MemoryFiles::new().with_file("/p/ctor.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = cpp_code_analysis::Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/ctor.cpp");
    let view = session
        .view_of_the_file(std::path::Path::new("/p/ctor.cpp"))
        .expect("a view");

    for scope in view.scopes.scopes() {
        println!(
            "{:?} range={:?} bindings={:?}",
            scope.kind,
            scope.range.map(|r| (r.start_offset, r.end_offset())),
            scope
                .bindings
                .iter()
                .map(|b| (b.name.text(), b.name_range.start_offset))
                .collect::<Vec<_>>()
        );
    }

    for (what, needle) in [
        ("the member-initializer list", ": first("),
        ("the body", "int here = "),
    ] {
        let at = source.find(needle).expect("the fixture writes it") + needle.len();
        let names = match session.name_completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(at),
        ) {
            Known::Yes(found) => found
                .names
                .iter()
                .map(|offered| offered.fact.name.clone())
                .collect::<Vec<_>>(),
            other => panic!("{what} should be answerable, got {other:?}"),
        };
        assert!(
            names.contains(&"f".to_string()),
            "the constructor's own parameter, in {what}: {names:?}"
        );
        assert!(
            names.contains(&"m".to_string()),
            "and the second one, in {what}: {names:?}"
        );
        assert!(
            names.contains(&"first".to_string()),
            "and the field it initialises, in {what}: {names:?}"
        );
    }
}
/// **A missing `;` is a parse error and has to reach the client.**
///
/// Reported: `std::cout << middleName << std::endl` with no semicolon produced no diagnostic, however long the user
/// waited. The question this pins is the first one — whether the answer exists at all — because a missing diagnostic
/// is either "nothing was found" or "it was found and never published", and the two are fixed in different places.
#[test]
fn a_missing_semicolon_is_reported() {
    let source = "int g();\nstruct Name {\n    void print(int i) {\n        std::cout << middleName << std::endl\n    }\n};\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = cpp_code_analysis::Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    println!(
        "before the edit: {:?}",
        session
            .diagnostics(std::path::Path::new("/p/a.cpp"))
            .map(|found| found.errors.len())
    );

    // What a user typing does: the session is told the file's new text.
    session.did_open(std::path::Path::new("/p/a.cpp"), source);
    println!(
        "right after the edit: {:?}",
        session
            .diagnostics(std::path::Path::new("/p/a.cpp"))
            .map(|found| found.errors.len())
    );
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    println!(
        "after re-indexing: {:?}",
        session
            .diagnostics(std::path::Path::new("/p/a.cpp"))
            .map(|found| found.errors.len())
    );

    let found = session
        .diagnostics("/p/a.cpp")
        .expect("the file is held, so it has an answer");
    println!(
        "reading={:?} errors={} unplaced={}",
        found.reading,
        found.errors.len(),
        found.unplaced
    );
    for error in &found.errors {
        println!("  {error:?}");
    }
    assert!(
        found
            .errors
            .iter()
            .any(|error| error.message.contains(';') || error.message.contains("expected")),
        "the missing `;` is a parse error and nothing else: {:?}",
        found
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
    );
}

/// **A macro is not a function** — its completion row carries no signature.
///
/// Reported from an editor: `FULL_NAME_FORMAT` is `#define FULL_NAME_FORMAT "Full name: {}"` and the completion
/// listed it with the detail `int ()`. A detail comes from `DeclFact`'s `returns`/`parameter_list`, and a macro has
/// neither — so the row was built from a fact whose four "what type does this declare" questions had been answered
/// about a *different* declaration: the macro's name offset is inside no declaration shape, so the shape lookup that
/// answers those questions found whatever enclosed it — here `int main()`, hence `int` and `()`.
#[test]
fn a_macro_is_not_shown_with_a_signature() {
    let source = "#define FULL_NAME_FORMAT \"Full name: {}\"\nint main() { FULL_NAME_FORMAT }\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    let view = cooked_view(&mut session, "/p/a.cpp");

    let cursor = source.rfind("FULL_NAME_FORMAT").expect("the fixture") + "FULL_NAME_FORMAT".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
    );
    let item = offered
        .items
        .iter()
        .find(|item| item.label == "FULL_NAME_FORMAT")
        .expect("the macro is offered");
    println!(
        "item: kind={:?} detail={:?} fact={:?}",
        item.kind,
        item.detail,
        item.fact
            .as_ref()
            .map(|fact| (&fact.kind, &fact.returns, &fact.parameter_list))
    );
    assert_eq!(
        item.detail,
        None,
        "a macro has no returns and no parameters: {:?}",
        item.fact
            .as_ref()
            .map(|fact| (&fact.kind, &fact.returns, &fact.parameter_list))
    );

    // **And its identity is the macro's own position.** The item a client resolves is a `(file, offset)` pair taken
    // from `name_range`, while the fact behind it is a clone of the file's first declaration — so a clone that kept
    // the template's range named `int main()`, and the popup showed `FULL_NAME_FORMAT` with the detail **`int ()`**.
    // Asserted on the offset rather than on the detail because the detail is read back by the server
    // (`completionItem/resolve`) from this offset: this layer is what has to give it the right one.
    let fact = item.fact.as_ref().expect("a macro item carries its fact");
    let at = source.find("FULL_NAME_FORMAT").expect("the fixture");
    assert_eq!(
        fact.name_range.start_offset, at,
        "the item's identity is the macro's name, not the template declaration's"
    );
    assert_eq!(
        fact.range.start_offset, at,
        "and its declaration range is the same place"
    );
    assert_eq!(fact.scope, None, "a `#define` is at file scope");
}

/// **The typed word is the range the client replaces, and the prefix the server filters on** — one report, both
/// halves.
///
/// From an editor, with `#include`s above the cursor:
///
/// ```text
///   auto x = a|        the list has every name in scope          ← no prefix, so nothing was filtered
///   auto x = an|       the list has only the keywords            ← the prefix was "n", and `anotherVec` does not
///                                                                  begin with "n"
///   accepting the item at `a|` wrote `aanotherVec`               ← the replace range was empty
/// ```
///
/// The three follow from one thing: the range the analysis answers with is in the **rendering's** coordinates,
/// while the client is looking at the file — and the two differ by everything the cook removed, which here is two
/// `#include` lines. See `text_edit` in the server, which converts; this test pins what the analysis answers, so
/// that a conversion with nothing to convert is not mistaken for the answer.
#[test]
fn the_typed_prefix_is_the_word_being_replaced() {
    let header =
        "namespace std { template <class T> struct vector { void push_back(const T&); }; }\n";

    // **One session per typed word**, because the word is part of the file: the cursor at `a|` and the cursor at
    // `an|` are two different texts, and a fixture that writes only `a` cannot be asked the second.
    for typed in ["a", "an"] {
        let source = format!(
            "#include \"v.h\"\nint main()\n{{\n    std::vector<int> anotherVec;\n    auto x = {typed}\n    return 0;\n}}\n"
        );
        let files = MemoryFiles::new()
            .with_file("/p/v.h", header)
            .with_file("/p/a.cpp", &source);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([
            std::path::PathBuf::from("/p/v.h"),
            std::path::PathBuf::from("/p/a.cpp"),
        ]);
        let view = cooked_view(&mut session, "/p/a.cpp");

        let at = source.find("auto x = ").expect("the fixture") + "auto x = ".len() + typed.len();
        match session.members_of(&view, "Source") {
            cpp_code_analysis::Known::Yes(list) => println!(
                "members_of(Source) → {:?}",
                list.members
                    .iter()
                    .map(|m| m.fact.name.clone())
                    .collect::<Vec<_>>()
            ),
            other => println!("members_of(Source) → {other:?}"),
        }
        let offered = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(at),
        );
        let labels: Vec<&str> = offered
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        println!(
            "`{typed}|`: prefix {:?} replace {}..{} (len {}) → {:?}",
            offered.prefix,
            offered.replace.start_offset,
            offered.replace.end_offset(),
            offered.replace.length,
            &labels[..labels.len().min(10)]
        );
        assert_eq!(
            offered.prefix, typed,
            "the prefix is what is being typed, so that the server filters on it"
        );
        assert_eq!(
            offered.replace.length,
            typed.len(),
            "and the range covers exactly that word, so that accepting an item replaces it"
        );
        assert!(
            labels.contains(&"anotherVec"),
            "`anotherVec` begins with `{typed}`: {:?}",
            &labels[..labels.len().min(12)]
        );
    }
}

#[test]
fn a_statement_position_prefix_is_read_too() {
    for typed in ["a", "an"] {
        let source =
            format!("int main()\n{{\n    int anotherVec = 1;\n    {typed}\n    return 0;\n}}\n");
        let files = MemoryFiles::new().with_file("/p/a.cpp", &source);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
        let view = cooked_view(&mut session, "/p/a.cpp");

        let at = source.find(&format!("    {typed}\n")).expect("the fixture") + 4 + typed.len();
        match session.members_of(&view, "Source") {
            cpp_code_analysis::Known::Yes(list) => println!(
                "members_of(Source) → {:?}",
                list.members
                    .iter()
                    .map(|m| m.fact.name.clone())
                    .collect::<Vec<_>>()
            ),
            other => println!("members_of(Source) → {other:?}"),
        }
        let offered = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(at),
        );
        let labels: Vec<&str> = offered
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        println!(
            "statement `{typed}|`: prefix {:?} replace {}..{} (len {}) → {:?}",
            offered.prefix,
            offered.replace.start_offset,
            offered.replace.end_offset(),
            offered.replace.length,
            &labels[..labels.len().min(10)]
        );
        assert_eq!(
            offered.prefix, typed,
            "the statement position reads the word too"
        );
        assert_eq!(offered.replace.length, typed.len());
        assert!(
            labels.contains(&"anotherVec"),
            "{:?}",
            &labels[..labels.len().min(12)]
        );
    }
}

#[test]
fn a_macro_that_expands_to_a_scope_offers_its_members() {
    let source = "\
#define STD std::
namespace std { int cout; int cin; int size; }
int main()
{
    STD s
    return 0;
}
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    let view = cooked_view(&mut session, "/p/a.cpp");
    println!("rendering: {:?}", view.source);

    let at = source.find("    STD s").expect("the fixture") + "    STD s".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!(
        "after the macro `STD s|`: prefix {:?} scope {:?} replace {}..{} → {:?}",
        offered.prefix,
        offered.scope,
        offered.replace.start_offset,
        offered.replace.end_offset(),
        &labels[..labels.len().min(8)]
    );

    // **The macro is expanded before the question is asked**, which is the point: `STD` is `std::` in the cook, so
    // the position is a *qualified* name with `s` written — the scope is `std` and the names are its members.
    assert_eq!(
        offered.scope, "std",
        "the expansion made this a qualified name"
    );
    assert_eq!(offered.prefix, "s");
    assert_eq!(
        offered.replace.length, 1,
        "the range covers the `s` that was typed"
    );
    assert!(labels.contains(&"size"), "{labels:?}");
    assert!(
        !labels.contains(&"main"),
        "and the file own names are not in `std`: {labels:?}"
    );
}

/// **A documentation comment answers two questions**: the command vocabulary after `@`, and the name the command
/// takes after `@param`.
///
/// Reported from an editor: "in comments there is no completion for doxygen commands, and if the command is
/// `param`, no completion for the parameter names". Before this the whole of a comment was refused — a `//` is
/// prose to every other question `context_at` asks, and the refusal was one line above every answer.
///
/// The parameter names come from the **fact of the declaration below the comment**, which is why the fixture writes
/// one: `DeclFact::parameter_list` is the spelling, and the names are read out of it the same way the inlay hints
/// read them, so a default argument and a `const T&` are the same question here as there.
#[test]
fn a_documentation_comment_offers_its_commands_and_their_names() {
    let source = "\
/// Frobnicates the widget.
/// @par
/// @param 
int frobnicate(const Widget& widget, int count);

struct Widget { int size; };
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    let view = cooked_view(&mut session, "/p/a.cpp");

    // **After `@par`** — the vocabulary, filtered by what was typed.
    let after_par = source.find("/// @par").expect("the fixture") + "/// @par".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(after_par),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!(
        "after `@par`: prefix {:?} → {:?}",
        offered.prefix,
        &labels[..labels.len().min(8)]
    );
    assert_eq!(
        offered.prefix, "par",
        "the command's name is what is being typed"
    );
    assert!(
        labels.contains(&"param"),
        "`param` is a command that begins with `par`: {labels:?}"
    );

    // **After `@param `** — the parameters of the declaration the comment documents, and nothing else.
    let after_param = source.find("/// @param ").expect("the fixture") + "/// @param ".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(after_param),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!(
        "after `@param `: prefix {:?} → {:?}",
        offered.prefix, labels
    );
    assert_eq!(offered.prefix, "", "nothing is typed yet");
    assert!(labels.contains(&"widget"), "{labels:?}");
    assert!(labels.contains(&"count"), "{labels:?}");
    assert!(
        !labels.contains(&"size"),
        "a member of a later class is not a parameter of this function: {labels:?}"
    );
}

/// **`#define` 的替换列表里补全它自己的形参** —— 那些名字只存在于这一行,索引和 scope 都不知道它们。
#[test]
fn a_macro_body_offers_its_own_parameters() {
    let source =
        "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#define MAX3(a, b, c) MAX(MAX(a, b), c)\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    let view = cooked_view(&mut session, "/p/a.cpp");

    // Inside the second replacement list, on the `b` of `MAX(a, b)`.
    let at = source
        .find("MAX3(a, b, c) MAX(MAX(a, b)")
        .expect("the fixture")
        + "MAX3(a, b, c) MAX(MAX(a, ".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!("prefix {:?} → {labels:?}", offered.prefix);
    // **The macro's parameters first**, then the macros already defined — at the start of the replacement list,
    // which is where a reader begins to write one.
    assert_eq!(labels, vec!["a", "b", "c", "MAX", "MAX3"], "{labels:?}");
    assert_eq!(
        offered.replace.length, 0,
        "nothing is typed yet, so the range is where the caret is"
    );
}

/// **`#if` / `#elif` / `#ifdef` 里补全宏** —— 条件在任何声明存在之前就被求值,所以那里只有宏和 `defined`。
#[test]
fn a_conditional_directive_offers_the_macros() {
    let source = "\
#define VERSION 3
#define FEATURE 1

#if VERSION > 2 && defined(FEATURE)
int newer;
#elif defined(FEATU)
int older;
#else
int neither;
#endif
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    let view = cooked_view(&mut session, "/p/a.cpp");

    // After `defined(` in the `#elif`: the macros are the answer, filtered by `FEATU`.
    let at = source.find("#elif defined(FEATU").expect("the fixture") + "#elif defined(FEATU".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!("prefix {:?} at {at} → {labels:?}", offered.prefix);
    assert_eq!(offered.prefix, "FEATU", "the word being typed");
    assert_eq!(labels, vec!["FEATURE"], "and only the macro that matches");

    // Right after `#if `: every macro the file defines, plus `defined`.
    let blank = source.find("#if VERSION").expect("the fixture") + "#if ".len();
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(blank),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!("blank prefix {:?} → {labels:?}", offered.prefix);
    assert!(labels.contains(&"VERSION"), "{labels:?}");
    assert!(labels.contains(&"defined"), "{labels:?}");
    assert!(
        !labels.contains(&"newer"),
        "a variable is not a word a condition can mean: {labels:?}"
    );
}

/// **半写完的 `it->` 也要有成员补全** —— 解析器在 `->` 后面没有名字时报错,补全必须从文本恢复这个访问。
#[test]
fn a_half_typed_arrow_still_offers_the_members() {
    let source = "\
#include \"map.h\"
#include \"string.h\"
int f() {
    std::map<int, std::string> myMap;
    auto it = myMap.begin();
    it->
    return 0;
}
";
    let map = "namespace std { template <class K, class V> struct map { }; template <class A, class B> struct pair { A first; B second; }; }\n";
    let string = "namespace std { template <class C> struct basic_string { }; typedef basic_string<char> string; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/map.h", map)
        .with_file("/p/string.h", string)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/map.h"),
        std::path::PathBuf::from("/p/string.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.index_everything();
    let view = cooked_view(&mut session, "/p/a.cpp");

    let at = source.find("it->\n").expect("the fixture") + "it->".len();
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
        .expect("the cursor is in the rendering");
    println!(
        "context: {:?}",
        cpp_code_analysis::completion::context_at(&view.root, reading.get())
    );
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!(
        "why: {}",
        cpp_code_analysis::completion::why_no_members(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            reading
        )
    );
    println!("after `it->`: {labels:?}");
    assert!(
        labels.contains(&"first") && labels.contains(&"second"),
        "the element's members: {labels:?}"
    );
}

/// **The original problem**: in a `.cpp` of separated definitions, a completion in the body of `Source::GetLine` cannot name
/// `Source`'s members.
///
/// # What was measured, and why this is not fixed here
///
/// The chain at that cursor is `[Function, Class("Source"), TranslationUnit]` once `names_a_scope_this_file_has` accepts a
/// qualifier this file has not built, and `members_of("Source")` then answers `["GetLine"]` — so the two structural pieces do
/// work when forced. Forcing them **breaks an invariant**: `sema::resolve::tests::a_qualifier_that_names_nothing_is_unknown_
/// rather_than_guessed` fails, because synthesising a class scope from a qualifier is exactly the guess that test forbids.
///
/// So the fix is **not** in the scope tree. The completion path should ask the question directly — "the declarator that
/// encloses this cursor is `Source::GetLine`, so the members of `Source` are in scope here" — using the declarator node and
/// `members_of`, which is what `CppDeclaration`/`CppDeclSpecifierSeq` already expose. That keeps the resolver's rule (a
/// qualifier names nothing until something declares it) and gives the completion what it needs.
#[test]
fn a_member_is_completed_in_a_separated_definition() {
    let header = "class Source {\npublic:\n    int _offset;\n    int GetLine(int n);\n};\n";
    let source = "#include \"a.h\"\nint Source::GetLine(int n) {\n    return _;\n}\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", header)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.index_everything();
    let view = cooked_view(&mut session, "/p/a.cpp");
    let at = source.find("return _").expect("the fixture") + "return _".len();
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
        .expect("the cursor is in the rendering");
    if let Some(innermost) = view.scopes.scope_at(reading.get()) {
        for id in view.scopes.scope_chain(innermost) {
            if let Some(scope) = view.scopes.scope(id) {
                println!(
                    "  chain: kind {:?} name {:?} prefix {:?}",
                    scope.kind,
                    scope.name,
                    view.scopes.qualification_prefix_of(id)
                );
            }
        }
    }
    match session.members_of(&view, "Source") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members_of(Source) → {:?}",
            list.members
                .iter()
                .map(|m| m.fact.name.clone())
                .collect::<Vec<_>>()
        ),
        other => println!("members_of(Source) → {other:?}"),
    }
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    println!("offered: {labels:?}");
    // `_offset` is a member too, but a name beginning with `_` inside a body is the implementation's by this crate's rule
    // (see `unused`'s note), so the assertion is the plain-named member — which is what the completion had none of before.
    assert!(
        labels.contains(&"_offset"),
        "the body of a separated definition names the class's own members: {labels:?}"
    );
}
/// **`->` reaches the class the arrow means**, which is not the object's own class.
///
/// Reported: "operator 的支持还没做完，比如智能指针的补全没有考虑到指向的内容". A smart pointer's own members are
/// `operator->`, `operator*`, `reset`, … — none of which `->` can reach — so a list built from the object's class is
/// the wrong list twice over: it offers what the arrow cannot name and omits what it means.
///
/// The chain is followed through the type: a pointer's pointee, or a class's own `operator->` return type and then its
/// pointee. `Widget* pointer;` and `std::unique_ptr<Widget> smart;` therefore answer the same member list.
#[test]
fn an_arrow_reaches_the_class_through_the_pointer_and_through_operator_arrow() {
    let source = "\
namespace std {
template <class T>
struct unique_ptr {
    T* operator->() const;
    T& operator*() const;
};
}
struct Widget {
    int size;
    void draw();
};
Widget* pointer;
std::unique_ptr<Widget> smart;
void f() {
    pointer->;
    smart->;
}
";
    let mem = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), mem);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();

    for (needle, what) in [
        ("pointer->", "a raw pointer"),
        ("smart->", "a smart pointer"),
    ] {
        let at = source.find(needle).expect("the fixture writes it") + needle.len();
        let labels = labels_at(&mut session, "/p/a.cpp", at);
        println!("{what} at {at}: {labels:?}");
        assert!(
            labels.iter().any(|label| label == "size"),
            "{what}: the pointee's member is offered: {labels:?}"
        );
        assert!(
            labels.iter().any(|label| label == "draw"),
            "{what}: and its member function: {labels:?}"
        );
        assert!(
            !labels.iter().any(|label| label == "operator->"),
            "{what}: the pointer's own `operator->` is not what the arrow reaches: {labels:?}"
        );
    }
}

/// **A `.` typed on a pointer lists the pointee's members**, which is what the reader meant — the operator they
/// should have typed is the client's business, and the list is the same one `->` gives.
#[test]
fn a_dot_on_a_pointer_lists_the_pointee() {
    let source = "\
struct Widget {
    int size;
};
Widget* pointer;
void f() {
    pointer.;
}
";
    let mem = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), mem);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();
    let at = source.find("pointer.").expect("the fixture") + "pointer.".len();
    let labels = labels_at(&mut session, "/p/a.cpp", at);
    println!("dot at {at}: {labels:?}");
    assert!(
        labels.iter().any(|label| label == "size"),
        "the pointee's member is offered after a `.` on a pointer: {labels:?}"
    );
}
/// **A `.` on a class with `operator->` is the same mistake as a `.` on a pointer** — the list is the pointee's and the
/// operator is replaced.
///
/// Reported with a screenshot: `config.Editorconfig.` where `Editorconfig` is a `std::shared_ptr<LuaEditorConfig>`
/// offered `operator=`, `element_type` and `weak_type` — the members of `shared_ptr` itself, which the reader is not
/// writing and which the `.` cannot use. A smart pointer reaches its pointee only through `operator->`, so the answer
/// has to follow it: the same chain the `->` path already walks, entered one operator earlier.
#[test]
fn a_dot_on_a_class_with_operator_arrow_lists_the_pointee() {
    let source = "\
namespace std {
template <class T>
struct shared_ptr {
    T* operator->() const;
    long use_count() const;
};
}
struct LuaEditorConfig {
    bool code_style_check;
    void OpenFile();
};
struct Config {
    std::shared_ptr<LuaEditorConfig> Editorconfig;
};
void f(Config config) {
    config.Editorconfig.;
}
";
    let mem = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), mem);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();

    let at =
        source.find("config.Editorconfig.").expect("the fixture") + "config.Editorconfig.".len();
    let labels = labels_at(&mut session, "/p/a.cpp", at);
    println!("dot on a smart pointer at {at}: {labels:?}");
    assert!(
        labels.iter().any(|label| label == "code_style_check"),
        "the pointee's member is offered: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label == "use_count"),
        "and the smart pointer's own members are not: {labels:?}"
    );
}

/// **A `.` typed on a pointer is completed as `->`**: the range the client replaces covers the operator itself, and
/// the insertion carries `->`.
///
/// Reported as "我们通过.补全的时候也应该补全时替换为->". The list is the pointee's (see the test above), and the operator
/// is the one thing the reader got wrong — `.size` on a `Widget*` does not compile — so the answer replaces it rather
/// than completing a member onto text that cannot use one.
#[test]
fn a_dot_on_a_pointer_is_completed_as_an_arrow() {
    let source = "\
struct Widget {
    int size;
};
Widget* pointer;
void f() {
    pointer.;
}
";
    let mem = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), mem);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let _ = session.view("/p/a.cpp");
    session.index_everything();

    // **The last one**: the fixture declares `pointer` and then uses it, and the use is what the cursor is on.
    let dot = source.rfind("pointer.").expect("the fixture") + "pointer".len();
    let at_in_the_file = dot + 1;
    let view = cooked_view(&mut session, "/p/a.cpp");
    let answered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at_in_the_file),
    );
    // **The answer's offsets are the rendering's**, and the rendering is not the file byte for byte (the cook drops
    // what the preprocessor consumed). The cursor therefore has to be read in the same numbering before the two are
    // compared — the mistake this test made first, and the reason both numbers are printed below.
    let at = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            at_in_the_file,
        ))
        .expect("the offset is in the rendering")
        .get();
    println!(
        "replace {:?} items {:?}",
        answered.replace,
        answered
            .items
            .iter()
            .map(|item| (&item.label, &item.insert))
            .collect::<Vec<_>>()
    );

    // **A rendering's offsets, not the fixture's**: the answer is in the cooked text's numbering, which is what the
    // client's range is in. What matters is the character the range covers and where it sits relative to the cursor.
    assert_eq!(
        answered.replace.length, 1,
        "the replacement covers one character — the operator: {:?}",
        answered.replace
    );
    assert_eq!(
        at - answered.replace.start_offset,
        1,
        "and that character is the one immediately before the cursor: {:?} at {at}",
        answered.replace
    );
    assert_eq!(
        &view.source[answered.replace.start_offset..answered.replace.end_offset()],
        ".",
        "which is the `.` the reader typed"
    );
    let size = answered
        .items
        .iter()
        .find(|item| item.label == "size")
        .expect("the pointee's member is offered");
    assert_eq!(
        size.insert, "->size",
        "and the insertion carries the operator the member can actually follow"
    );
}

/// **`->` is not `.`** — the flag the completion of a member follows `operator->` with.
///
/// `MemberAccess::arrow` was hard-coded `false` by the text fallback, so a cursor after `ptr->` was answered as if
/// the reader had typed `ptr.`: a class whose members live behind `operator->` was looked up for its **own** members.
/// The operator's own spelling is the only evidence there is, and it is one token to read.
#[test]
fn an_arrow_is_read_as_an_arrow() {
    let text = "void f(Widget* p, Widget v) {\n    p->\n    v.member\n}\n";
    let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
    let root = tree.get_red_root();

    let arrow =
        cpp_code_analysis::completion::context_at(&root, text.find("p->").expect("fixture") + 3);
    match arrow {
        cpp_code_analysis::completion::CompletionContext::Member(access) => {
            assert!(
                access.arrow,
                "the operator written is `->`, so this is an arrow access"
            );
            assert!(
                access.member.is_empty(),
                "nothing is typed after the operator yet"
            );
        }
        other => panic!("a member access is what follows an arrow, not {other:?}"),
    }

    // And the other spelling: `v.member` is a dot, and the member is written.
    let dot = cpp_code_analysis::completion::context_at(
        &root,
        text.find("v.member").expect("fixture") + 4,
    );
    match dot {
        cpp_code_analysis::completion::CompletionContext::Member(access) => {
            assert!(!access.arrow, "a `.` is not an arrow");
            assert_eq!(access.member, "member");
        }
        other => panic!("a member access is what follows a dot, not {other:?}"),
    }
}

/// **A member answer says whether it is missing members**, and a client has to be told.
///
/// `isIncomplete` in the protocol means one thing — *ask again later* — and a member list has two ways of being
/// short: the budget (`truncated`, where asking again returns the same list) and the **analysis** (`incomplete`:
/// a class this answer depends on has not been read). Measured on a real project: `std::string* message; message->`
/// answered **one** item on the first keystroke (`operator basic_string_view`, all the raw reading of `<xstring>` had
/// at file scope) and **58** once the header was cooked — and the one-item list went out with `isIncomplete: false`,
/// so the client showed it as the answer. See `docs/completion.md`.
#[test]
fn a_member_answer_that_is_missing_facts_is_marked_incomplete() {
    // **A class nothing declares**: the honest answer is "not yet", not "no members".
    let unknown = "\
int f() {
    no_such_class* p = nullptr;
    p->
    return 0;
}
";
    let (session, view) = one_file_session(unknown);
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at_the_end_of(unknown, "p->")),
    );
    assert!(offered.items.is_empty(), "{:?}", offered.items);
    assert!(
        offered.incomplete,
        "a class this analysis cannot see is a missing fact, so the client must be allowed to ask again"
    );

    // **A class it can see**: complete, and the flag says so.
    let known = "\
struct Widget { int size; int grow; };
int f() {
    Widget* p = nullptr;
    p->
    return 0;
}
";
    let (session, view) = one_file_session(known);
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at_the_end_of(known, "p->")),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert_eq!(labels, vec!["grow", "size"], "the pointee's members");
    assert!(
        !offered.incomplete,
        "nothing is missing here, so the list stands as it is"
    );
}

/// **A smart pointer's arrow reaches the pointee, through `element_type`** — the standard's pointer-like protocol.
///
/// `<memory>`'s `unique_ptr::operator->` returns `pointer`, a member typedef whose target is a dependent
/// `typename _Get_deleter_pointer_type<…>::type`: an engine that does not instantiate metafunctions cannot follow
/// that declaration, and before this it fell back to the *smart pointer's own* members. Measured on a real project:
/// `std::unique_ptr<int> p; p->` offered `get`, `get_deleter` and `operator*` — none of which can be written after
/// that arrow — and `std::unique_ptr<std::string> p; p->` offered nothing at all.
///
/// `element_type` is what `std::pointer_traits` is written in terms of, so this is the standard's answer rather than
/// a guess about one class; a class that is not pointer-like declares none, and nothing else is affected.
#[test]
fn a_smart_pointers_arrow_reaches_what_element_type_names() {
    let header = "\
namespace std {
template <class T> struct pointers {
    typedef typename T::pointer pointer;
};
template <class T> struct unique_ptr {
    T* stored;
    typedef T element_type;
    typedef typename pointers<T>::pointer pointer;
    pointer operator->() const;
    T& operator*() const;
    int get() const;
};
struct string { int size() const; const char* c_str() const; };
}
";
    let source = "\
#include \"smart.h\"
int f() {
    std::unique_ptr<std::string> p;
    p->
    return 0;
}
";
    let files = MemoryFiles::new()
        .with_file("/p/smart.h", header)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/smart.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.index_everything();
    let view = cooked_view(&mut session, "/p/a.cpp");

    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at_the_end_of(source, "p->")),
    );
    let labels: Vec<&str> = offered
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert!(
        labels.contains(&"size") && labels.contains(&"c_str"),
        "`element_type` says the arrow reaches `std::string`: {labels:?}"
    );
    assert!(
        !labels.contains(&"get"),
        "the smart pointer's own members cannot be written after its arrow: {labels:?}"
    );
}

/// One file in one project, cooked, with a view of it — the shape the two tests above share.
fn one_file_session(source: &str) -> (Session<MemoryFiles>, cpp_code_analysis::FileView) {
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let view = cooked_view(&mut session, "/p/a.cpp");
    (session, view)
}

/// The file offset just after the first `needle` — where a cursor sits when the reader has typed it.
fn at_the_end_of(source: &str, needle: &str) -> usize {
    source.find(needle).expect("the fixture writes it") + needle.len()
}

/// **A pointee that is not a class has no members, and that answer is final.**
///
/// `int* p; p->` walks the pointer, finds `int`, and `int` names no class — so the list is empty *and complete*. The
/// distinction is the protocol's: an empty list marked `isIncomplete` tells a client to ask again, and here nothing
/// can change, so the popup would stand empty for ever. It is where `std::vector<int> v; v.begin()->` lands too: the
/// iterator's arrow reaches `int`.
#[test]
fn a_pointee_that_is_not_a_class_answers_nothing_and_is_complete() {
    let source = "\
int f() {
    int* p = nullptr;
    p->
    return 0;
}
";
    let (session, view) = one_file_session(source);
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at_the_end_of(source, "p->")),
    );
    assert!(offered.items.is_empty(), "{:?}", offered.items);
    assert!(
        !offered.incomplete,
        "`int` is not missing anything: the list is final"
    );
}

/// A session over one **open** file, drained — the state a completion is asked in.
///
/// `did_open` rather than [`cooked_view`], and the difference is not incidental: a *cook* answers a question about a
/// file from its summary, and the rendering that comes back is condensed onto one line (measured here:
/// `"struct Widget { int size; }; void f() { Widget * p; p-> Widget w; w. }"` for a five-line file). A completion is
/// asked about the file the reader has **open**, whose rendering is its own text with its own line breaks — and line
/// breaks are what a recovery rule can read. See the module documentation of `cpp_parser`'s `exprs.rs` on the member
/// name that must be on the operator's line.
fn a_pointer_fixture(text: &'static str) -> (Session<MemoryFiles>, cpp_code_analysis::FileView) {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", text);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.did_open("/p/a.cpp", text);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 400 {
        session.advance(16);
        rounds += 1;
    }
    let view = session
        .view("/p/a.cpp")
        .expect("the open file's own reading");
    (session, view)
}

/// **An arrow on a pointer inserts the member's name, and only the name.**
///
/// Measured before this: `Widget* p; p->` answered the pointee's members with `insert: "->length"` and a replacement
/// range grown one byte to the left, over the `>` — accepting an item wrote `p-->length`. The rewrite is for the one
/// case where the operator typed and the answer disagree, a `.` on a pointer; on `->` the reader already typed the
/// right operator, so the name is the whole insertion and the range is empty where the name goes.
#[test]
fn an_arrow_on_a_pointer_inserts_the_name_and_not_the_operator() {
    let text = "struct Widget { int size; int length; };\nvoid f()\n{\nWidget * p;\np->\n}\n";
    let (session, view) = a_pointer_fixture(text);
    let at = text.find("p->").expect("the fixture writes it") + 3;
    let set = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );

    let mut labels: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        ["length", "size"],
        "the pointee's members, not names: {set:?}"
    );
    for item in &set.items {
        assert_eq!(
            item.insert, item.label,
            "the arrow is the reader's, not part of what a member inserts"
        );
    }
    assert_eq!(
        (set.replace.start_offset, set.replace.length),
        (at, 0),
        "nothing is written after the operator, so the range is empty where the name goes"
    );
}

/// **A `.` on a pointer is still rewritten to `->`** — the one case the rewrite is for.
///
/// `p.size` on a `Widget*` does not compile, and C++ has no such member; the list is the pointee's, which is what the
/// reader meant, and the operator they typed is the half that is wrong. So the insertion gains `->` and the range
/// grows one byte left to cover the `.`.
#[test]
fn a_dot_on_a_pointer_still_rewrites_the_operator() {
    let text = "struct Widget { int size; };\nvoid f()\n{\nWidget * p;\np.\n}\n";
    let (session, view) = a_pointer_fixture(text);
    let at = text.find("p.\n").expect("the fixture writes it") + 2;
    let set = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );

    assert_eq!(set.items.len(), 1, "one member: {set:?}");
    assert_eq!(
        set.items[0].insert, "->size",
        "the operator is part of the insertion"
    );
    assert_eq!(
        (set.replace.start_offset, set.replace.length),
        (at - 1, 1),
        "the `.` is replaced along with the name's position"
    );
}

/// **`co_await` has the type its awaiter's `await_resume()` returns.**
///
/// ```cpp
/// task<payload> greet();
/// task<int> use() { auto s = co_await greet(); s. }   // `s` is a `payload`
/// ```
///
/// The awaited expression is a `UnaryExpr` with a `co_await` token, and without an arm for it `s` was a type nobody
/// knew: measured on `test_cpp/coroutine.cpp`, completion after `auto s = co_await greet("x"); s.` answered
/// **nothing**, while the coroutine parsed, hovered and coloured correctly. The awaiter is the operand's own type
/// (`task<T>` is its own awaiter) and the value is `await_resume()`'s return type, read through the class's template
/// arguments — `T` becomes `payload` — which is the same substitution the member lookups already do.
#[test]
fn an_awaited_value_has_the_type_await_resume_returns() {
    let text = "\
struct payload {
    int size() const;
    int length() const;
};

template <typename T>
struct task {
    struct promise_type {
        task get_return_object() { return task{}; }
        std::suspend_always initial_suspend() { return {}; }
        std::suspend_always final_suspend() noexcept { return {}; }
        void return_value(T v) {}
        void unhandled_exception() {}
    };
    bool await_ready() const noexcept { return false; }
    void await_suspend(std::coroutine_handle<>) {}
    T await_resume() { return T{}; }
};

task<payload> greet();

task<int> use() {
    auto s = co_await greet();
    s.
    co_return 0;
}
";
    let (session, view) = a_pointer_fixture(text);
    let at = text.find("    s.\n").expect("the fixture writes it") + 6;
    let set = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );

    let mut labels: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        ["length", "size"],
        "the awaited value is a `payload`, so its members are what a completion offers: {set:?}"
    );
}

/// **A member typedef of a class template names the class it points at.**
///
/// ```cpp
/// template <typename T> struct outer { using handle_type = body; };
/// outer<int>::handle_type h;  h.        // the members of `body`, not of `outer`
/// ```
///
/// The spelling of such a type is `outer<int>::handle_type`, and two readers cut it at the **first** `<`:
/// `base_type_name` — which is what `members_of` starts from, so the class asked about was `outer` — and
/// `alias_target_of`, whose head split exists for an *alias template's own* arguments (`enable_if_t<int, double>`).
/// Measured: the completion offered `outer`'s own typedefs (`handle_type`) while the same class written without the
/// argument answered the target's members, and a coroutine's `task<int>::handle_type h; h.` offered `await_ready`/
/// `take` instead of `resume`/`promise`.
///
/// The three shapes below are one test because they share the one helper: stripping a **qualifier's** argument list
/// must leave an alias template's own arguments alone.
#[test]
fn a_member_typedef_of_a_class_template_names_its_target() {
    let plain = "\
struct body {
    int size() const;
    int length() const;
};
struct outer {
    using handle_type = body;
};
void f() { outer::handle_type h{}; h. }
";
    assert_eq!(
        member_labels_at(plain, "h."),
        ["length", "size"],
        "a class member typedef, without arguments"
    );

    let templated = "\
struct body {
    int size() const;
    int length() const;
};
template <typename T>
struct outer {
    using handle_type = body;
};
void f() { outer<int>::handle_type h{}; h. }
";
    assert_eq!(
        member_labels_at(templated, "h."),
        ["length", "size"],
        "the same typedef on a class template: the argument belongs to the use, not to the name"
    );

    // A member typedef whose target is itself a template-id, which is what a coroutine's `handle_type` is.
    let coroutine = "\
namespace std {
template <typename P>
struct coroutine_handle {
    static coroutine_handle from_promise(P &promise);
    void resume();
    void destroy();
    bool done() const;
    P &promise() const;
    void *address() const;
};
}
template <typename T>
struct task {
    struct promise_type { };
    using handle_type = std::coroutine_handle<promise_type>;
};
void f() { task<int>::handle_type h{}; h. }
";
    let labels = member_labels_at(coroutine, "h.");
    for wanted in ["resume", "destroy", "promise", "address", "from_promise"] {
        assert!(
            labels.iter().any(|label| label.as_str() == wanted),
            "`task<int>::handle_type` is a `coroutine_handle`, so it has `{wanted}`: {labels:?}"
        );
    }

    // A **nested class** addressed through its outer class's template-id is the same spelling shape, and it must not
    // lose its own members to it: `outer<int>::promise_type` is a class declared inside `outer`.
    let nested_class = "\
template <typename T>
struct outer {
    struct promise_type {
        T get_return_object();
        std::suspend_always initial_suspend();
        T return_value(T v);
    };
};
void f() { outer<int>::promise_type p{}; p. }
";
    assert_eq!(
        member_labels_at(nested_class, "p."),
        ["get_return_object", "initial_suspend", "return_value"],
        "a nested class named through its outer class's template-id keeps its own members"
    );

    // …and an alias template's own arguments still substitute, which the same split is responsible for.
    let alias_template = "\
template <bool B, typename T = void>
struct enable_if { using type = T; };
template <bool B, typename T = void>
using enable_if_t = typename enable_if<B, T>::type;
struct payload { int size() const; };
void f() { enable_if_t<true, payload> p{}; p. }
";
    assert_eq!(
        member_labels_at(alias_template, "p."),
        ["size"],
        "an alias template's arguments are substituted, not stripped"
    );
}

/// The member labels offered after `needle` in a one-file fixture, sorted.
fn member_labels_at(source: &'static str, needle: &str) -> Vec<String> {
    let (session, view) = a_pointer_fixture(source);
    let at = source.find(needle).expect("the fixture writes it") + needle.len();
    let set = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(at),
    );
    let mut labels: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    labels.sort_unstable();
    labels
}
