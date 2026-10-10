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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
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
    let mut session = session_with_the_project();
    let view = cooked_view(&mut session, "/p/a.cpp");

    if std::env::var_os("CPPLS_TRACE_COORDINATES").is_some() {
        let reading = view.reading_offset_of(offset).expect("the cursor is in the rendering");
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
            in_file as i64 - reading as i64,
            SOURCE.find("local_variable + w").expect("the use") as i64
                - view.source.find("local_variable + w").expect("the use") as i64
        );
    }

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
    let view = cooked_view(&mut session, "/p/a.cpp");

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
    let mut session = session_with_the_project();
    let view = cooked_view(&mut session, "/p/a.cpp");

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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");
    let view = cooked_view(&mut session, "/p/a.cpp");

    // The **name** list, asked inside the body: the user's local is there, the reserved globals are not.
    let known = match session.name_completions(&view, FIXTURE.find("w.").expect("the fixture")) {
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
        FIXTURE.find("w.").expect("the fixture") + 2,
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
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
        .completions(&view, offset)
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
    let provenance: Vec<String> = match session.name_completions(&view, cursor) {
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
    let offered = session.completions(&view, cursor);

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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("inc").expect("the fixture") + "inc".len();
    let found = session.completions(&view, cursor);

    let include = found
        .items
        .iter()
        .find(|item| item.label == "include")
        .unwrap_or_else(|| panic!("`include` is offered: {:?}", found.items));

    assert_eq!(
        include.replace.start_offset,
        1,
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("#endif ").expect("the fixture") + "#endif ".len();

    assert!(
        session.completions(&view, cursor).items.is_empty(),
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("#define ").expect("the fixture") + "#define ".len();
    let labels = labels_at(&mut session, "/p/a.cpp", cursor);

    assert_eq!(
        session.completions(&view, cursor).prefix,
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
    let found = session.completions(&view, cursor);

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
    assert_eq!(near.replace.start_offset, typed.find("near").expect("the fixture"));
    assert_eq!(near.replace.length, 4, "the whole half-written name");
}

/// **A completion in a comment is empty**, and the same cursor one character earlier is not: the offset that
/// merely *touches* a comment's end is at the start of what follows it.
#[test]
fn a_comment_is_not_code() {
    const FIXTURE: &str = "int x; // a note here\n";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", FIXTURE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let inside = FIXTURE.find("note").expect("the fixture");

    assert!(
        session.completions(&view, inside).items.is_empty(),
        "a name in a comment is not code"
    );
    assert!(
        !session
            .completions(&view, FIXTURE.find('x').expect("the fixture") + 1)
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("w.").expect("the fixture") + 2;
    let found = session.completions(&view, cursor);

    let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
    assert!(labels.contains(&"size"), "{labels:?}");
    assert!(labels.contains(&"grow"), "{labels:?}");
    assert!(labels.contains(&"inherited"), "{labels:?}");
    assert!(
        !labels.contains(&"f"),
        "and not the names in scope, which cannot follow the operator: {labels:?}"
    );

    let size = position(&labels.iter().map(|it| it.to_string()).collect::<Vec<_>>(), "size");
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("one::").expect("the fixture") + "one::".len();
    let found = session.completions(&view, cursor);

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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.load("/p/a.cpp");

    let view = cooked_view(&mut session, "/p/a.cpp");
    let at_the_member = FIXTURE.find("w.size").expect("the fixture");
    let cursor = at_the_member + 2 + "si".len();
    let found = session.completions(&view, cursor);

    assert_eq!(found.prefix, "si", "what filters is the part before the cursor");
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
        !session.completions(&view, cursor).truncated,
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
    let Known::Yes(found) = session.header_at(&view, cursor) else {
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
            session.header_at(&view, RANKED_CPP.find("#include").expect("the fixture")),
            Known::Yes(_)
        ),
        "the `#` is not the header's name"
    );

    // A name that is not a header is **not** this query's business — and the answer says so rather than saying
    // "nothing found", which is what keeps it from swallowing a jump that has a real declaration behind it.
    let on_a_declaration = RANKED_CPP.find("at_file_scope").expect("the fixture");
    match session.header_at(&view, on_a_declaration) {
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    let view = cooked_view(&mut session, "/p/a.cpp");
    let cursor = FIXTURE.find("nowhere").expect("the fixture");

    match session.header_at(&view, cursor) {
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
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
    let view = cooked_view(&mut session, "/p/a.cpp");

    // **Without the catch-up**: the header's summary is the old one, so `Widget` still has only `size`.
    let stale = session.completions(&view, cursor);
    let stale_labels: Vec<&str> = stale.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        !stale_labels.contains(&"extra"),
        "the index is one edit behind until something reads the file: {stale_labels:?}"
    );

    // **With it**: one parse of one file, and the answer is about the text the user is looking at.
    session.catch_up(std::path::Path::new("/p/widget.h"));
    let fresh = session.completions(&view, cursor);
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([
        std::path::PathBuf::from("/p/main.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);

    // **The server's sequence, and only that**: the client opens the document, and the pump has not run.
    session.did_open("/p/main.cpp", FRESHNESS_USES_HEADER);
    session.catch_up(std::path::Path::new("/p/main.cpp"));

    let cursor = FRESHNESS_USES_HEADER.find("w.").expect("the fixture") + 2;
    let view = cooked_view(&mut session, "/p/main.cpp");
    let found = session.completions(&view, cursor);

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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
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
    let mut session =
        Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/widget.h"),
    ]);
    session.index_everything();

    let view = cooked_view(&mut session, "/p/a.cpp");
    let dot = FIXTURE.find("w.").expect("the fixture") + 1;
    assert_eq!(&FIXTURE[dot..dot + 1], ".", "the fixture's operator");

    for offset in [dot, dot + 1] {
        let found = session.completions(&view, offset);
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
        // always explained by the step that emptied it.
        let why = cpp_code_analysis::why_no_members(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset,
        );
        assert!(
            why.contains("the member query answered"),
            "the diagnosis agrees with the list at {offset}: {why}"
        );
    }
}
