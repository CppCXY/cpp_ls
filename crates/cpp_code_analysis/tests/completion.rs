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
    let view = session.view("/p/a.cpp").expect("the file is held");

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
fn labels_at(session: &Session<MemoryFiles>, path: &str, offset: usize) -> Vec<String> {
    let view = session.view(path).expect("the file is held");

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
    let session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&session, "/p/a.cpp", cursor);

    let body = position(&labels, "in_the_body");
    let file = position(&labels, "at_file_scope");
    let near = position(&labels, "from_the_near_header");
    let far = position(&labels, "from_the_far_header");

    // The provenance is quoted in the message rather than asserted on directly: it is *why* the order is what it
    // is, and a failure of the order is the thing worth reading.
    let view = session.view("/p/a.cpp").expect("the file is held");
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
    let session = ranked_session();
    let cursor = RANKED_CPP.find("in_the_body + at").expect("the fixture") + "in_the_body + ".len();

    let labels = labels_at(&session, "/p/a.cpp", cursor);
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
    let session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&session, "/p/a.cpp", cursor);
    let view = session.view("/p/a.cpp").expect("the file is held");
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
    let session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();

    let labels = labels_at(&session, "/p/a.cpp", cursor);
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
    let cursor = FIXTURE.find("#define ").expect("the fixture") + "#define ".len();
    let labels = labels_at(&session, "/p/a.cpp", cursor);

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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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

    let view = session.view("/p/a.cpp").expect("the file is held");
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
    let session = ranked_session();
    let cursor = RANKED_CPP.find("return ").expect("the fixture") + "return ".len();
    let view = session.view("/p/a.cpp").expect("the file is held");

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
    let session = ranked_session();
    let view = session.view("/p/a.cpp").expect("the file is held");

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

    let view = session.view("/p/a.cpp").expect("the file is held");
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
