//! The index's own tests — what a project's summaries answer, asked of a `ProjectIndex` built from memory.
//!
//! Moved out of `project/mod.rs` without a change. `use super::*` is how a test reaches the module's private
//! machinery, which is the point of testing it from here.

use super::{IncludeVisibility, ProjectIndex};
use crate::CompilerConfig;
use crate::CookedFile;
use crate::DeclKind;
use crate::FileIndexer;
use crate::MacroDefinitions;
use crate::Marked;
use crate::MemoryFiles;
use crate::NoMacroBodies;
use crate::TranslationUnit;
use crate::WalkCache;
use crate::build_scopes;
use crate::cache::SummaryKey;
use crate::index;
use crate::index::summarize;
use crate::preprocess;
use crate::summary;
use crate::symbol::{Known, UnknownReason};
use std::path::Path;

/// **A member named like its class does not hide the class** — the reader's report "`LSPHandle` was read as the
/// constructor itself rather than the class", as a test at the layer that broke it.
///
/// `LSPHandle.cpp` defines `LSPHandle::LSPHandle(int)`, so **this file itself** declares the name `LSPHandle` — in
/// the class's scope. The local-win rule then dropped every *other* file's fact with that name as "not local", and
/// the class the header declares went with them: `definition("LSPHandle")` was `Ambiguous` between the two
/// constructors, and `LSPHandle::RefreshDiagnostic` resolved its qualifier to a constructor. A candidate in a
/// different scope is a different entity, which is what the rule says now — and removing that clause makes this
/// test fail at the assertion below.
#[test]
fn a_member_named_like_its_class_does_not_hide_the_class() {
    let index = index(&[
        (
            "/p/LSPHandle.h",
            "class LSPHandle {\npublic:\n    LSPHandle(int server);\n    void RefreshDiagnostic();\n};\n",
        ),
        (
            "/p/LSPHandle.cpp",
            "#include \"LSPHandle.h\"\nLSPHandle::LSPHandle(int server) {\n}\n",
        ),
    ]);

    let found = index.definition("LSPHandle", Path::new("/p/LSPHandle.cpp"));
    let Known::Yes(definition) = found else {
        panic!("the class must resolve, not fall to its constructor: {found:?}");
    };
    assert_eq!(
        definition.fact.kind,
        DeclKind::Type,
        "the name `LSPHandle` resolved to {:?} in {}",
        definition.fact.kind,
        definition.file.display()
    );
}

fn index(files: &[(&str, &str)]) -> ProjectIndex {
    let mut index = ProjectIndex::new();

    for (path, source) in files {
        // The includes are resolved by hand here rather than through a `FileProvider`, because what these
        // tests are about is the graph that results — not the search that produced it.
        let mut summary = summarize(Path::new(path), source, SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);
    }

    index
}

#[test]
fn a_declaration_in_an_included_header_is_found() {
    let index = index(&[
        ("/p/widget.h", "struct Widget { int size; };\n"),
        (
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        ),
    ]);

    let found = index.definition("Widget", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("the class in the included header must be found: {found:?}");
    };

    assert_eq!(definition.file, Path::new("/p/widget.h"));
    assert_eq!(definition.fact.name, "Widget");
}

/// **A name only a compiler can see is a name the index answers for — once the file is cooked.**
///
/// `DECLARE_HANDLE(HWND)` declares `HWND__` and `HWND` to a compiler and nothing to a reader of the file's
/// own text, because the declaration is inside the macro's replacement list. So the raw index cannot find
/// either name, and the cooked reading — indexed and mapped back by `FileIndexer::index_rendering` — can.
/// This is the whole reason the index holds a second reading.
#[test]
fn a_declaration_a_macro_makes_is_found_once_the_file_is_cooked() {
    let files = [
        (
            "/p/decl.h",
            "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
             typedef struct name##__ *name\n",
        ),
        ("/p/api.h", "#include \"decl.h\"\nDECLARE_HANDLE(HWND);\n"),
    ];

    let mut index = ProjectIndex::new();
    let mut definitions = MacroDefinitions::default();
    for (path, source) in files {
        let mut summary = summarize(Path::new(path), source, SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        let _ = source;
        index.insert(summary);
    }

    let api = Path::new("/p/api.h");
    let source = files[1].1;
    let before = index.definition("HWND__", api);
    assert!(
        matches!(before, Known::Unknown(_)),
        "the raw reading cannot see a declaration inside a macro body: {before:?}"
    );

    // Cook the file **through a unit** — its environment is what makes the macro expand — and hand the index
    // what came out of it.
    let text_of = |wanted: &Path| {
        files
            .iter()
            .find(|(path, _)| Path::new(path) == wanted)
            .map(|(_, source)| *source)
            .unwrap_or("")
    };
    let unit = TranslationUnit::walk(
        index.summary(api).expect("indexed"),
        |wanted| {
            index
                .summary(wanted)
                .map(|summary| (summary, text_of(wanted)))
        },
        &Marked::default(),
        &mut definitions,
        false,
        &WalkCache::new(),
    );
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let unit_definitions = unit.definitions();
    let view = unit.environment_of(api).expect("the unit reaches api.h");
    let macros = preprocess::cooked::FileMacros::new(&view, &unit_definitions, None, true);
    let rendered = preprocess::cooked::cook_with(source, &tokens, &macros).render();

    let provider = MemoryFiles::new();
    let config = CompilerConfig::default();
    let indexer = FileIndexer::new(&provider, &config);
    let indexed = indexer.index_rendering(api, &rendered, SummaryKey::new(0, 0));
    assert_eq!(indexed.mapped.dropped, 0, "every range landed in the file");
    index.insert_cooked(api, indexed.into());

    // Now it can: the struct the macro declared, and the typedef beside it.
    let after = index.definition("HWND__", api);
    let Known::Yes(definition) = after else {
        panic!("the cooked reading declares it: {after:?}");
    };
    assert_eq!(definition.file, api);
    let written = &source[definition.fact.range.start_offset..definition.fact.range.end_offset()];
    assert!(
        written.contains("DECLARE_HANDLE"),
        "the declaration is reported at the invocation: {written:?}"
    );
    assert!(
        matches!(index.definition("HWND", api), Known::Yes(_)),
        "and the typedef with it: {:?}",
        index.definition("HWND", api)
    );
}

/// **The two readings do not double-count**: an ordinary declaration is in both, and a candidate list that
/// held it twice would answer `Ambiguous` for every name in the project.
#[test]
fn a_declaration_both_readings_found_is_one_candidate() {
    let source = "struct Plain { int size; };\n";
    let mut index = index(&[("/p/a.h", source)]);

    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = preprocess::cooked::cook(source, &tokens).render();
    let provider = MemoryFiles::new();
    let config = CompilerConfig::default();
    let indexer = FileIndexer::new(&provider, &config);
    let indexed = indexer.index_rendering(Path::new("/p/a.h"), &rendered, SummaryKey::new(0, 0));
    assert_eq!(indexed.mapped.dropped, 0);
    index.insert_cooked(Path::new("/p/a.h"), indexed.into());

    let found = index.definition("Plain", Path::new("/p/a.h"));
    assert!(matches!(found, Known::Yes(_)), "one candidate: {found:?}");
}

/// A cooked reading of `source`, as [`crate::Session::cook`] builds one: the rendering indexed and mapped back.
fn cooked_reading_of(path: &str, source: &str) -> CookedFile {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = preprocess::cooked::cook(source, &tokens).render();
    let provider = MemoryFiles::new();
    let config = CompilerConfig::default();
    let indexer = FileIndexer::new(&provider, &config);
    indexer
        .index_rendering(Path::new(path), &rendered, SummaryKey::new(0, 0))
        .into()
}

/// **A workspace search finds declarations in files nobody has opened**, ranks them, and leaves locals out.
///
/// The query is project-wide and deliberately *not* visibility-scoped: a symbol search is about the project, so
/// a class in a header nothing includes is still a symbol. What it does share with every other declaration query
/// is the two readings — a name only the cooked reading declares is a name a compiler knows.
#[test]
fn a_workspace_search_finds_symbols_across_the_project() {
    let mut index = index(&[
        (
            "/p/widget.h",
            "namespace ns {\nstruct Widget { int size; };\n}\n",
        ),
        (
            "/p/other.cpp",
            "void f() {\n    int widget_local;\n}\nstruct Widest { int y; };\n",
        ),
        (
            "/p/handle.h",
            "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\nDECLARE_HANDLE(HWND);\n",
        ),
    ]);

    let names = |found: &[super::ProjectSymbol]| -> Vec<String> {
        found
            .iter()
            .map(|symbol| symbol.fact.name.to_string())
            .collect()
    };

    // Case-insensitive; both names begin with the query, so they share a rank and are ordered by name.
    let found = index.symbols_matching("wid", 100);
    assert_eq!(
        names(&found),
        vec!["Widest", "Widget"],
        "one rank, alphabetical by name: {found:?}"
    );
    assert_eq!(
        found[1].fact.scope.as_deref(),
        Some("ns"),
        "and the fact says which scope it was written in: {found:?}"
    );

    // The rank comes before the name: the exact match is first although `Widest` sorts earlier than `Widget`.
    let found = index.symbols_matching("widget", 100);
    assert_eq!(names(&found), vec!["Widget"], "{found:?}");
    let found = index.symbols_matching("idg", 100);
    assert_eq!(
        names(&found),
        vec!["Widget"],
        "a substring finds the name it is inside: {found:?}"
    );

    // A local is not a project symbol, even though its name matches.
    assert_eq!(
        names(&index.symbols_matching("widget_local", 100)),
        Vec::<String>::new(),
        "a variable inside a body is not something the project has"
    );

    // A qualified query finds it too — and finds a **member**, which is the one query that should.
    assert_eq!(
        names(&index.symbols_matching("ns::widget", 100)),
        vec!["Widget"]
    );
    assert_eq!(
        names(&index.symbols_matching("Widget::si", 100)),
        vec!["size"],
        "a query with `::` is about a qualified name"
    );
    assert_eq!(index.symbols_matching("wid", 1).len(), 1);

    // **A name only the cooked reading declares is a symbol**: a compiler sees `HWND__`, so a search finds it.
    // The file has to be in the index for the reading to be reachable at all — `insert_cooked` files a *second*
    // reading of a file the index knows.
    index.insert_cooked(
        Path::new("/p/handle.h"),
        cooked_reading_of(
            "/p/handle.h",
            "#define DECLARE_HANDLE(name) struct name##__ { int unused; };\nDECLARE_HANDLE(HWND);",
        ),
    );
    assert!(
        names(&index.symbols_matching("HWND__", 100)).contains(&"HWND__".to_string()),
        "the macro-declared type is a symbol a compiler knows"
    );
}

/// **Re-indexing a file with *different* text takes its cooked reading with it.**
///
/// The ordinary invalidation drops both together ([`ProjectIndex::forget`] — an edit, a close, a watched file all
/// go through it), and this is the path with no `forget` in it: the file was read again and the bytes differ, so
/// its own declarations are replaced by that insert. A cooked reading left over from the old text would go on
/// answering for words nobody wrote any more, at offsets into them — a wrong answer rather than a missing one.
///
/// The second half is the guard that keeps this from being a cache defeat: the *same* text re-read (a cache hit,
/// a re-index of an unchanged file) is not a change, and the reading stays.
#[test]
fn re_indexing_different_text_takes_the_cooked_reading_with_it() {
    let source = "struct Plain { int size; };\n";
    let mut index = index(&[("/p/a.h", source)]);
    index.insert_cooked(Path::new("/p/a.h"), cooked_reading_of("/p/a.h", source));
    assert!(index.cooked_declarations(Path::new("/p/a.h")).is_some());

    // The same text, read again: the content hash matches, so the reading describes what is still there.
    let same = summarize(Path::new("/p/a.h"), source, SummaryKey::new(0, 0));
    index.insert(same);
    assert!(
        index.cooked_declarations(Path::new("/p/a.h")).is_some(),
        "an unchanged file keeps its reading"
    );

    // Different text: the reading described the old bytes, so it goes.
    let changed = summarize(
        Path::new("/p/a.h"),
        "struct Other { int size; };\n",
        SummaryKey::new(0, 0),
    );
    index.insert(changed);
    assert!(
        index.cooked_reading(Path::new("/p/a.h")).is_none(),
        "the reading goes with the text it was built from"
    );
}

#[test]
fn a_declaration_in_a_header_the_file_does_not_include_is_not_found() {
    // The distinction the whole visibility walk exists for: `Widget` is in the project, and not in scope
    // here. Answering `Yes` would be a jump to a declaration the file cannot compile against.
    let index = index(&[
        ("/p/widget.h", "struct Widget { int size; };\n"),
        ("/p/other.cpp", "void g() { }\n"),
    ]);

    let found = index.definition("Widget", Path::new("/p/other.cpp"));

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "a name that is not in scope is not a definition: {found:?}"
    );
}

#[test]
fn visibility_follows_a_chain_of_includes() {
    let index = index(&[
        ("/p/deep.h", "struct Deep { int x; };\n"),
        ("/p/middle.h", "#include \"deep.h\"\n"),
        (
            "/p/main.cpp",
            "#include \"middle.h\"\nvoid f() { Deep d; }\n",
        ),
    ]);

    let found = index.definition("Deep", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("a transitively included declaration is visible: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/deep.h"));
}

#[test]
fn the_files_own_declaration_wins_over_an_included_one() {
    // C++ resolves to the declaration in the file being compiled, and a jump that went into a header instead
    // would be to a different entity.
    let index = index(&[
        ("/p/widget.h", "struct Widget { int from_header; };\n"),
        (
            "/p/main.cpp",
            "#include \"widget.h\"\nstruct Widget { int from_this_file; };\n",
        ),
    ]);

    let found = index.definition("Widget", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("the local declaration wins: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/main.cpp"));
}

#[test]
fn a_name_declared_in_two_visible_headers_is_ambiguous_rather_than_guessed() {
    let index = index(&[
        ("/p/one.h", "int count;\n"),
        ("/p/two.h", "int count;\n"),
        (
            "/p/main.cpp",
            "#include \"one.h\"\n#include \"two.h\"\nvoid f() { count = 1; }\n",
        ),
    ]);

    let found = index.definition("count", Path::new("/p/main.cpp"));

    assert!(
        matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
        "two declarations and nothing to choose between them: {found:?}"
    );
}

#[test]
fn two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one() {
    // The fixture above, through the plural query: `Ambiguous` is the honest answer to "which one", and a
    // client that can show a peek list never has to be told it — both of these are real answers.
    let index = index(&[
        ("/p/one.h", "int count;\n"),
        ("/p/two.h", "int count;\n"),
        (
            "/p/main.cpp",
            "#include \"one.h\"\n#include \"two.h\"\nvoid f() { count = 1; }\n",
        ),
    ]);

    let Known::Yes(found) = index.definitions("count", Path::new("/p/main.cpp")) else {
        panic!("two declarations, both of them answers");
    };

    assert_eq!(found.found.len(), 2);
    assert_eq!(found.conditional, 0);
    assert_eq!(
        found.found[0].file,
        Path::new("/p/one.h"),
        "the order is imposed (by file, then offset), so a client's peek list cannot reorder itself between \
         two identical requests"
    );
    assert_eq!(found.found[1].file, Path::new("/p/two.h"));
}

/// **A bare name does not reach into a class** — the scope half of the key a name lookup needs.
///
/// Reported from an editor: ctrl-click on `printf` in a `.cpp` that includes a header declaring
/// `struct Name { void printf(int); };` and `void Name::printf(int i)` offered a peek list of **three** — the
/// member's declaration, its out-of-class definition, and the global `printf` from the C library. The first two
/// are a function the `.cpp` cannot name without a `Name::` or an object, so following them lands on a
/// declaration that does not describe the call.
///
/// The two directions are what make this a rule rather than a filter: the member's own definition still
/// answers its member (the cursor writes `Name::printf`), and the **unfiltered** query still holds all three —
/// that is the list a member access with an unknown object type asks for.
#[test]
fn a_bare_name_does_not_reach_a_class_member() {
    let index = index(&[
        (
            "/p/name.h",
            "struct Name {\n    void printf(int i);\n};\nvoid Name::printf(int i) { }\n",
        ),
        ("/p/stdio.h", "void printf(const char* format, ...);\n"),
        (
            "/p/main.cpp",
            "#include \"name.h\"\n#include \"stdio.h\"\nvoid f() { printf(\"x\"); }\n",
        ),
    ]);

    let found = index.definitions_in_scope("printf", Path::new("/p/main.cpp"));
    let Known::Yes(found) = found else {
        panic!("the file can name the global `printf`: {found:?}");
    };
    assert_eq!(
        found.found.len(),
        1,
        "one declaration, the only one an unqualified `printf` can denote: {:?}",
        found
            .found
            .iter()
            .map(|definition| definition.file.display().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(found.found[0].file, Path::new("/p/stdio.h"));

    // **The unfiltered list is what a member fallback wants**, and the member is in it.
    //
    // **Two, not three.** The declaration and its out-of-class definition are *one declaration recorded twice*
    // by this index's own rule (`the_same_declaration_in_one_file`: the same file, the same name and scope, and
    // the parameter list that tells two functions apart, all present and equal). They became mergeable when the
    // grammar learned the shape — before that the definition had no name of its own, so nothing about it could be
    // compared with the declaration's. What this assertion is *for* is the fallback, and that still sees
    // `Name::printf`.
    let Known::Yes(every) = index.definitions("printf", Path::new("/p/main.cpp")) else {
        panic!("the index holds the spelling");
    };
    assert_eq!(
        every.found.len(),
        2,
        "a member access whose object's type is unknown still asks this one"
    );

    // **A qualified spelling keeps the member**, which is how one is named from outside the class — and it is
    // now *one* answer for the reason above; the survivor is the declaration, which is earlier in the file.
    let Known::Yes(qualified) =
        index.definitions_in_scope("Name::printf", Path::new("/p/main.cpp"))
    else {
        panic!("a qualified name is a member's own question");
    };
    assert_eq!(
        qualified.found.len(),
        1,
        "the member, addressed through its qualifier: {qualified:?}"
    );
}

#[test]
fn an_overload_set_is_a_list_and_a_namespace_is_one_entity() {
    // The two shapes the measurement on a real file produced — 46 identifiers answered `Ambiguous` there, and
    // the lists behind them were 2, 3, 4 … 17 declarations. `find` in `std::basic_string` is seventeen of them,
    // every one an answer; `std` is **58**, all of them the *same* namespace, which the language reopens
    // rather than redeclares. So the list is not simply "what the index found".
    let index = index(&[
        (
            "/p/lib.h",
            "namespace ns {\n  void f(int);\n  void f(double);\n}\n",
        ),
        ("/p/other.h", "namespace ns {\n  void f(char);\n}\n"),
        (
            "/p/main.cpp",
            "#include \"lib.h\"\n#include \"other.h\"\nvoid h() { ns::f(1); }\n",
        ),
    ]);

    let Known::Yes(found) = index.definitions("ns::f", Path::new("/p/main.cpp")) else {
        panic!("an overload set is three declarations, not an ambiguity");
    };
    assert_eq!(found.found.len(), 3);

    let Known::Yes(found) = index.definitions("ns", Path::new("/p/main.cpp")) else {
        panic!("the namespace is declared in both headers");
    };
    assert_eq!(
        found.found.len(),
        1,
        "one namespace, however many files reopen it: a peek list of fifty-eight entries answers a question \
         nobody asked"
    );
}

#[test]
fn one_function_declared_twice_is_one_entity_and_an_overload_is_two() {
    // The rule that needed a parameter list to exist at all. `the_same_declaration_in_one_file` demands
    // **evidence** that can tell two declarations apart before it merges them, and for a function that evidence
    // is the parameter list — which the model did not record. So the gate `!parameters.is_empty()` was asking
    // about the *template* parameter names, which an ordinary function never has: **no function was ever
    // merged**, and the two halves of MSVC's `<iostream>` idiom (`void f(int);` written plain and again behind
    // `_EXPORT_STD`) stayed two answers where one is the truth.
    //
    // Both directions in one fixture, because a rule that merged everything would pass the first half alone:
    // the same declaration twice is one entity, and `(double)` beside `(int)` is a second one.
    let index = index(&[(
        "/p/lib.h",
        "namespace ns {\n  void f(int);\n  void f(int);\n  void f(double);\n}\n",
    )]);

    let Known::Yes(found) = index.definitions("ns::f", Path::new("/p/lib.h")) else {
        panic!("three declarations of `ns::f`, and two entities among them");
    };

    assert_eq!(
        found.found.len(),
        2,
        "the twice-declared `(int)` is one entity; `(double)` is an overload: {found:?}"
    );
}

/// **The two visibility walks are the same answer**, which is the only thing the cheaper one had to prove.
///
/// The walk that carries macro state replaced one that asked `environment::visibility_at` per guarded edge, and
/// the replacement is worth two orders of magnitude on a standard-library closure (114 closure walks → one
/// traversal; 1428 ms → 18 ms of a query measured on the latency fixture). What it must not be is a *different*
/// answer: the visibility set is what a completion list is built from, so a set that lost a file is a missing
/// name rather than a faster query.
///
/// Four shapes, because they are the four ways the two can disagree — an unguarded edge, a guarded one nobody
/// can decide, one that is decided and false, and a file reached twice by routes of different visibility.
#[test]
fn the_walk_that_carries_the_state_sees_what_asking_each_condition_saw() {
    let index = index(&[
        ("/p/one.h", "#define ONE 1\n"),
        ("/p/two.h", "int count;\n"),
        ("/p/three.h", "int three;\n"),
        (
            "/p/main.cpp",
            "#include \"one.h\"\n#ifdef FEATURE\n#include \"two.h\"\n#endif\n#include \"three.h\"\n",
        ),
        // …and a second route to `two.h`, unguarded, which is the case the crossing has to record rather than
        // the entry: `visited` enters a file once, so the guarded route is the one that descends.
        ("/p/four.h", "#include \"two.h\"\n"),
        ("/p/other.cpp", "#include \"four.h\"\n"),
    ]);

    for root in ["/p/main.cpp", "/p/other.cpp"] {
        let walked = index.visible_files(Path::new(root));
        let asked = index.visible_files_by_asking_each_condition(Path::new(root));

        let mut walked = walked.clone();
        let mut asked = asked.clone();
        walked.sort_by(|left, right| left.0.cmp(&right.0));
        asked.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(
            walked, asked,
            "the two walks disagree about {root}: the completion list is built from this set"
        );
    }
}

#[test]
fn a_declaration_behind_a_conditional_include_is_counted_not_offered() {
    // Whether a name reached through an `#if` is in scope depends on macros this layer does not have, so it is
    // not offered as a jump target — and it is not dropped either: "one declaration" and "one declaration and
    // one that might also be there" are different answers.
    let index = index(&[
        ("/p/one.h", "int count;\n"),
        ("/p/two.h", "int count;\n"),
        (
            "/p/main.cpp",
            "#include \"one.h\"\n#ifdef FEATURE\n#include \"two.h\"\n#endif\nvoid f() { count = 1; }\n",
        ),
    ]);

    let Known::Yes(found) = index.definitions("count", Path::new("/p/main.cpp")) else {
        panic!("the unguarded header declares it");
    };

    assert_eq!(found.found.len(), 1);
    assert_eq!(found.found[0].file, Path::new("/p/one.h"));
    assert_eq!(
        found.conditional, 1,
        "and the one that might also be there is counted, so the answer does not claim there is nothing else"
    );
}

#[test]
fn an_include_inside_a_condition_that_is_decided_and_false_is_not_visible() {
    // The rule that turned a 14351-item completion list into a 692-item one. MSVC's `<intrin.h>` includes
    // `<arm_neon.h>`/`<arm64_neon.h>` under `#if defined(_M_ARM64)`, and `<immintrin.h>` pulls in
    // `zmmintrin.h` for AVX-512 — all of them **decided, and false**, for a file compiled for x64. While an
    // `Inactive` edge was downgraded to `Conditional` instead of dropped, every ARM and AVX-512 intrinsic was
    // "visible" from a hello-world file, and therefore offered to the user: 8 of every 10 completion items.
    //
    // `#if 0` is the same shape with nothing to evaluate: decided, false, and no macro knowledge needed.
    let index = index(&[
        ("/p/arm.h", "int arm_intrinsic;\n"),
        (
            "/p/main.cpp",
            "#if 0\n#include \"arm.h\"\n#endif\nvoid f() { }\n",
        ),
    ]);

    let found = index.definition("arm_intrinsic", Path::new("/p/main.cpp"));
    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "the branch was not taken, so the file is not in this translation unit at all: {found:?}"
    );
}

#[test]
fn a_qualified_name_prefers_the_declaration_that_matches_it() {
    let index = index(&[
        ("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n"),
        ("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n"),
        (
            "/p/main.cpp",
            "#include \"a.h\"\n#include \"b.h\"\nvoid f() { a::Widget w; }\n",
        ),
    ]);

    let found = index.definition("a::Widget", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("the qualified name must select one of them: {found:?}");
    };

    assert_eq!(definition.file, Path::new("/p/a.h"));
    assert_eq!(definition.fact.qualified_name(), "a::Widget");
}

#[test]
fn a_guarded_include_makes_the_answer_unknown() {
    // `#include` inside an `#if`: whether it is taken depends on macros the index does not have, so the
    // honest answer is that the name may or may not be in scope.
    let index = index(&[
        ("/p/widget.h", "struct Widget { int size; };\n"),
        (
            "/p/main.cpp",
            "#if defined(USE_WIDGET)\n#include \"widget.h\"\n#endif\nvoid f() { Widget w; }\n",
        ),
    ]);

    let found = index.definition("Widget", Path::new("/p/main.cpp"));

    assert!(
        matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
        "a conditional include cannot be decided here: {found:?}"
    );
}

#[test]
fn an_unconditional_include_beats_a_guarded_one() {
    // The same name reachable two ways: the unguarded path is always there, so it is the answer.
    let index = index(&[
        ("/p/real.h", "struct Widget { int size; };\n"),
        (
            "/p/main.cpp",
            "#include \"real.h\"\n#if defined(X)\n#include \"other.h\"\n#endif\nvoid f() { Widget w; }\n",
        ),
        ("/p/other.h", "struct Widget { int other; };\n"),
    ]);

    let found = index.definition("Widget", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("the unconditional path wins: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/real.h"));
}

#[test]
fn a_file_that_includes_nothing_sees_only_itself() {
    let index = index(&[("/p/a.cpp", "int count;\n"), ("/p/b.cpp", "int count;\n")]);

    for path in ["/p/a.cpp", "/p/b.cpp"] {
        let found = index.definition("count", Path::new(path));
        let Known::Yes(definition) = found else {
            panic!("{path} must find its own count: {found:?}");
        };
        assert_eq!(definition.file, Path::new(path));
    }
}

#[test]
fn a_cycle_of_includes_does_not_hang_the_visibility_walk() {
    let index = index(&[
        ("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n"),
        ("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n"),
        ("/p/main.cpp", "#include \"a.h\"\nvoid f() { B b; }\n"),
    ]);

    let found = index.definition("B", Path::new("/p/main.cpp"));
    let Known::Yes(definition) = found else {
        panic!("a cycle must not stop a name being found: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/b.h"));
}

#[test]
fn the_reverse_edges_are_derived_from_the_summaries() {
    let index = index(&[
        ("/p/header.h", "int x;\n"),
        ("/p/one.cpp", "#include \"header.h\"\n"),
        ("/p/two.cpp", "#include \"header.h\"\n"),
    ]);

    let mut includers = index.includers_of(Path::new("/p/header.h"));
    includers.sort();
    assert_eq!(
        includers,
        [Path::new("/p/one.cpp"), Path::new("/p/two.cpp")],
        "an edit to the header invalidates exactly these"
    );
}

#[test]
fn reindexing_a_file_removes_the_edges_its_old_text_had() {
    // An edge that outlived the `#include` that wrote it would keep a header reachable from a file that no
    // longer includes it — a stale conclusion with nothing on disk to contradict it.
    let mut index = index(&[
        ("/p/widget.h", "struct Widget { int size; };\n"),
        (
            "/p/main.cpp",
            "#include \"widget.h\"\nvoid f() { Widget w; }\n",
        ),
    ]);

    assert!(index.includers_of(Path::new("/p/widget.h")).len() == 1);

    let mut edited = summarize(
        Path::new("/p/main.cpp"),
        "void f() { }\n",
        SummaryKey::new(1, 0),
    );
    for include in &mut edited.includes {
        include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
    }
    index.insert(edited);

    assert!(
        index.includers_of(Path::new("/p/widget.h")).is_empty(),
        "the edge must go with the line that wrote it"
    );
    assert!(
        matches!(
            index.definition("Widget", Path::new("/p/main.cpp")),
            Known::Unknown(_)
        ),
        "and the name is no longer visible there"
    );
}

#[test]
fn visibility_is_reported_so_a_caller_can_downgrade() {
    let index = index(&[
        ("/p/widget.h", "struct Widget { int size; };\n"),
        ("/p/main.cpp", "#include \"widget.h\"\n"),
    ]);

    let found = index.files_declaring("Widget", Path::new("/p/main.cpp"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].visibility, IncludeVisibility::Unconditional);
}

/// The offset of the last `needle`, which is the cursor position in these fixtures.
fn at(source: &str, needle: &str) -> usize {
    source
        .rfind(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
}

/// An index over the fixtures **plus** the analysed querying file, which is what
/// [`super::definition_across_files`] needs: the index holds the other files, and the scope tree holds this
/// one.
fn analysed(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
    let (index, tree) = analysed_while_typing(files, from, source);

    assert!(
        tree.get_errors().is_empty(),
        "the fixture must parse cleanly: {source:?}"
    );

    (index, tree)
}

/// The same, for a file that is **being typed** and therefore need not parse cleanly.
///
/// Separate from [`analysed`] rather than a flag on it, because the two assertions mean different things: a
/// shape assertion is worthless if the tree it reads came from a file with errors, while an incomplete file is
/// the *state the completion query exists for* — `w.` is not a program, it is a question. What is deliberately
/// **not** relaxed is that the querying file is indexed: the index and the tree have to describe the same text
/// or the offsets in the answer mean something else.
fn analysed_while_typing(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
    let mut index = index(files);

    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());

    let mut summary = summarize(Path::new(from), source, SummaryKey::new(0, 0));
    for include in &mut summary.includes {
        include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
    }
    index.insert(summary);

    (index, tree)
}

#[test]
fn a_local_declaration_is_answered_without_looking_in_the_index() {
    // The first step of the resolution order, and the one that has to win: a local shadows a header's
    // declaration, so a jump that went into the header would be to a different entity.
    let source = "#include \"widget.h\"\nvoid f() {\n  int count = 0;\n  count = 1;\n}\n";
    let (index, tree) = analysed(&[("/p/widget.h", "int count;\n")], "/p/main.cpp", source);

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "count = 1;"),
    );

    let Known::Yes(definition) = found else {
        panic!("the local must win: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/main.cpp"));
    assert!(
        definition.fact.range.start_offset > at(source, "void f"),
        "the jump goes to the local, not to the header"
    );
}

#[test]
fn another_files_local_is_never_the_answer() {
    // The wrong answer this exists to prevent, and it is the *common* one rather than a corner: a header's
    // function bodies declare thousands of names (`__first`, `__n`, `_Tp`), and every one of them is a
    // declaration in the index with no scope to place it in. A lookup that matched one would answer with a
    // name the reader cannot see — and, worse, a name that is not the entity the cursor is asking about.
    let source = "#include \"widget.h\"\nvoid f() {\n  helper();\n}\n";
    let (index, tree) = analysed(
        &[(
            "/p/widget.h",
            "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
        )],
        "/p/main.cpp",
        source,
    );

    // The fact itself, so that a failure says which half broke: the header's declaration is marked local.
    let header = index.summary(Path::new("/p/widget.h")).expect("indexed");
    let helper = header
        .declarations
        .iter()
        .find(|fact| fact.name == "helper")
        .expect("the header declares `helper`");
    assert!(helper.local, "declared inside `g`'s body");
    assert_eq!(helper.scope, None, "…which is why the scope cannot say so");

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "helper();"),
    );

    assert!(
        matches!(
            found,
            Known::Unknown(UnknownReason::NotDeclaredHere(_)) | Known::No
        ),
        "the header's local is not a declaration of the name this file writes: {found:?}"
    );

    // And the other half, so that the skip is a filter rather than a hole: a *file-scope* declaration in the
    // same header is still found through the index.
    let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
    let (index, tree) = analysed(
        &[(
            "/p/widget.h",
            "void g() {\n  int helper = 0;\n}\nstruct Widget { int size; };\n",
        )],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "Widget w;"),
    );

    let Known::Yes(definition) = found else {
        panic!("`Widget` is at file scope in the header: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/widget.h"));
    assert!(!definition.fact.local);
}

#[test]
fn a_call_resolves_through_the_return_type_of_a_header() {
    // The whole chain, and three of its links are in the header rather than in the buffer: the class, the
    // member function, and the class its return type names. This is what `DeclFact::returns` was added for,
    // and it is also the path a cached summary goes through — the field is written and read by the codec.
    let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.inner().size = 1;\n}\n";
    let (index, tree) = analysed(
        &[(
            "/p/widget.h",
            "struct Inner {\n  int size;\n};\nstruct Widget {\n  Inner inner();\n};\n",
        )],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::member_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "size = 1;"),
    );

    let Known::Yes(definition) = found else {
        panic!("`w.inner()` returns an `Inner`, which declares `size`: {found:?}");
    };
    assert_eq!(definition.fact.name, "size");
    assert_eq!(
        definition.file,
        Path::new("/p/widget.h"),
        "the member is in the header, like everything else on this chain"
    );
}

#[test]
fn a_name_only_a_header_declares_is_found_through_the_index() {
    let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
    let (index, tree) = analysed(
        &[("/p/widget.h", "struct Widget { int size; };\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "Widget w;"),
    );

    let Known::Yes(definition) = found else {
        panic!("the header's class must be found: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/widget.h"));
    assert_eq!(definition.fact.name, "Widget");
}

#[test]
fn a_qualified_name_resolves_into_an_included_header() {
    // The two layers together: the qualifier names a namespace that only the *header* declares, so the
    // single-file layer cannot answer it and the spelling goes to the index — which can, because a fact
    // records the scope it was written in.
    let source = "#include \"a.h\"\nvoid f() {\n  a::Widget w;\n}\n";
    let (index, tree) = analysed(
        &[("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "Widget w;"),
    );

    let Known::Yes(definition) = found else {
        panic!("the header's `a::Widget` must be found: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/a.h"));
    assert_eq!(definition.fact.qualified_name(), "a::Widget");
}

#[test]
fn a_qualified_name_is_not_answered_by_a_same_named_declaration_elsewhere() {
    // The wrong answer qualification exists to prevent, one file further out: `b::Widget` is in scope (its
    // header is included), and the cursor asked for `a::Widget`, which is not. Answering with `b`'s would be a
    // jump to a different entity that the user cannot tell apart from the right one.
    let source = "#include \"b.h\"\nvoid f() {\n  a::Widget w;\n}\n";
    let (index, tree) = analysed(
        &[("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "Widget w;"),
    );

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "`a::Widget` is not declared anywhere this file can see: {found:?}"
    );
}

#[test]
fn a_global_spelling_does_not_match_a_namespaced_declaration() {
    // `::Widget` means the global name space. Both declarations exist and both are visible, so a matcher that
    // dropped the `::` would have two candidates and no way to choose — the prefix is what chooses.
    let source = "#include \"both.h\"\nvoid f() {\n  ::Widget w;\n}\n";
    let (index, tree) = analysed(
        &[(
            "/p/both.h",
            "struct Widget { int global; };\nnamespace ns {\n  struct Widget { int nested; };\n}\n",
        )],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "Widget w;"),
    );

    let Known::Yes(definition) = found else {
        panic!("the global `Widget` is the one asked for: {found:?}");
    };
    assert_eq!(definition.fact.scope, None);
    assert_eq!(definition.fact.name, "Widget");
}

// -------------------------------------------------------------------------------------------
// Member access
//
// The first query that needs a *type*. Every fixture here declares the object and its type in the same
// file, because that is where the interesting failure is — the lookup that would otherwise happen is
// "find some `size` in scope", and a test that only ever has one `size` in the project cannot tell the
// difference between typing the object and guessing.
// -------------------------------------------------------------------------------------------

/// The member a `needle` position names, with the file analysed and indexed the way a real query has it.
fn member_of(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
    needle: &str,
) -> Known<super::ProjectDefinition> {
    let (index, tree) = analysed(files, from, source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    super::member_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new(from),
        at(source, needle),
    )
}

// -------------------------------------------------------------------------------------------
// typedef / using aliases
//
// The shape the standard library is written in: `std::string` **is** `std::basic_string<char>` and has no
// members of its own, so a lookup that stops at the spelling finds nothing at all. The motivating case is in
// these tests in its real form — a `typedef` inside a namespace, with the target written relative to it.
// -------------------------------------------------------------------------------------------

#[test]
fn a_member_of_an_alias_resolves_through_it() {
    let source = "struct Widget {\n  int size;\n};\nusing Alias = Widget;\n\
                  void f() {\n  Alias w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`Alias` names `Widget`, so `size` is `Widget::size`: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
}

#[test]
fn a_member_of_the_standard_library_string_resolves_into_basic_string() {
    // The case: `basic_string` is where every member is declared, and the
    // alias's target is written **unqualified** inside `namespace std`, so following it needs the alias's own
    // scope. Getting that wrong looks up a global `basic_string` and finds nothing.
    let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n    \
                  int find(int);\n  };\n  typedef basic_string<char> string;\n}\n\
                  void f() {\n  std::string s;\n  s.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`std::string::size` is `std::basic_string::size`: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(
        member.fact.scope.as_deref(),
        Some("std::basic_string"),
        "the member is declared in the class the alias points at, and its scope says so"
    );
}

#[test]
fn an_alias_written_with_using_resolves_the_same_way() {
    let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n  };\n  \
                  using string = basic_string<char>;\n}\n\
                  void f() {\n  std::string s;\n  s.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the `using` form is the same declaration shape: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("std::basic_string"));
}

#[test]
fn an_alias_target_written_with_a_qualifier_is_taken_as_it_stands() {
    // A qualified target is already a complete spelling, so the alias's scope is not prepended: doing it would
    // ask for `ns::other::Thing`, which nothing declares.
    let source = "namespace other {\n  struct Thing {\n    int size;\n  };\n}\n\
                  namespace ns {\n  using Alias = other::Thing;\n}\n\
                  void f() {\n  ns::Alias t;\n  t.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the qualified target is the class: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("other::Thing"));
}

#[test]
fn an_alias_to_an_alias_is_followed_to_the_class() {
    // Two steps, which is what the bound exists for: `A` → `B` → the class.
    let source = "struct Widget {\n  int size;\n};\nusing B = Widget;\nusing A = B;\n\
                  void f() {\n  A w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("two aliases deep is still a class at the end: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
}

#[test]
fn a_cycle_of_aliases_terminates_and_says_nothing_was_declared() {
    // `using A = B; using B = A;` is writable, so the walk has to stop on its own rather than follow the pair
    // for ever. The answer is the honest one: nothing in the chain names a class.
    let source = "using A = B;\nusing B = A;\nvoid f() {\n  A w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(_)),
        "a cycle of aliases names no class: {found:?}"
    );
}

#[test]
fn an_alias_to_itself_terminates() {
    let source = "using A = A;\nvoid f() {\n  A w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(_)),
        "`using A = A;` names no class: {found:?}"
    );
}

#[test]
fn an_alias_whose_target_is_not_indexed_reports_the_target() {
    // The target is a real spelling even when nobody indexed it, and the answer says so rather than reporting
    // the alias: `Widget::size` is the question that could not be answered, not `Alias::size`.
    let source = "using Alias = Widget;\nvoid f() {\n  Alias w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Unknown(UnknownReason::NotDeclaredHere(written)) = found else {
        panic!("the target is what was looked for: {found:?}");
    };
    assert_eq!(&*written, "Widget::size");
}

#[test]
fn an_alias_to_a_non_class_is_not_a_member_lookup() {
    // `typedef int MyInt;` — following it lands on `int`, which names no class, so the answer is "not
    // declared" rather than a jump into whatever `int` might be mistaken for.
    let source = "typedef int MyInt;\nvoid f() {\n  MyInt x;\n  x.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "`int` declares no members: {found:?}"
    );
}

#[test]
fn a_function_pointer_typedef_records_the_whole_type() {
    // `typedef void (*F)(int);` — the type is the specifiers *and* the declarator with the alias's own name
    // cut out, which is `void (*)(int)`. Reading only the specifiers would give `void`, and reading the
    // declarator without cutting would give a type whose *name* is `F`.
    let source = "typedef void (*F)(int);\nvoid f() {\n  F callback;\n  callback();\n}\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let Known::Yes(fact) = index.definition("F", Path::new("/p/a.cpp")) else {
        panic!("the alias is a fact")
    };
    assert_eq!(
        fact.fact.type_of.as_deref(),
        Some("void (*)(int)"),
        "specifiers plus declarator minus the name"
    );

    // And a `typedef` of a plain type needs no such surgery.
    let plain = "typedef basic_string<char> String;\n";
    let (index, tree) = analysed(&[], "/p/b.cpp", plain);
    let Known::Yes(fact) = index.definition("String", Path::new("/p/b.cpp")) else {
        panic!("the alias is a fact")
    };
    assert_eq!(fact.fact.type_of.as_deref(), Some("basic_string<char>"));

    let _ = (root, tree);
}

#[test]
fn a_complex_type_keeps_the_adjective_in_its_spelling() {
    // **`_Complex` is an adjective on the floating type**, not a type of its own: the two words are one type. The
    // file-scope shape used to be read as a `MacroCall` followed by a separate `double z;`, so the recorded type
    // was `double`; both spellings now record the whole type.
    let source = "_Complex double z;\ndouble _Complex w;\n";
    let (index, _tree) = analysed(&[], "/p/a.c", source);

    let Known::Yes(z) = index.definition("z", Path::new("/p/a.c")) else {
        panic!("`z` is a declaration")
    };
    assert_eq!(z.fact.type_of.as_deref(), Some("_Complex double"));
    let Known::Yes(w) = index.definition("w", Path::new("/p/a.c")) else {
        panic!("`w` is a declaration")
    };
    assert_eq!(w.fact.type_of.as_deref(), Some("double _Complex"));
}

#[test]
fn a_gnu_label_declaration_binds_labels_not_variables() {
    // **`__label__` names live in the label namespace.** Read as an ordinary declaration, `done` was a variable
    // whose type was `__label__`; the label namespace is the one `goto` and `done:` share, so the fact must be the
    // same kind a label statement produces (`Other`) and carry no type.
    let source = "void f(void) {\n  __label__ done, again;\n  goto done;\n}\n";
    let summary = summarize(Path::new("/p/a.c"), source, SummaryKey::new(0, 0));

    for name in ["done", "again"] {
        let fact = summary
            .declarations
            .iter()
            .find(|declaration| declaration.name == name)
            .unwrap_or_else(|| panic!("`{name}` is declared by the `__label__` line"));
        assert_eq!(
            fact.kind,
            summary::DeclKind::Other,
            "`{name}` is a label, not a variable"
        );
        assert_eq!(fact.type_of, None, "a label writes no type");
    }
}

#[test]
fn a_member_list_of_an_alias_is_the_member_list_of_its_target() {
    // The other query that turns a spelling into a class, and it has to agree with the single-member one:
    // completion for `std::string` that listed nothing would be a member list of the alias rather than of the
    // class, and an alias has no members.
    let source = "namespace std {\n  template<typename T> struct basic_string {\n    int size;\n    \
                  int find(int);\n  };\n  typedef basic_string<char> string;\n}\n";
    let list = members_of_class(&[], "/p/a.cpp", source, "std::string");

    let Known::Yes(list) = list else {
        panic!("the alias's target is a class with members: {list:?}");
    };
    assert!(
        names(&list).contains(&"size") && names(&list).contains(&"find"),
        "the members are `basic_string`'s: {:?}",
        names(&list)
    );
}

#[test]
fn a_member_access_resolves_through_the_objects_type() {
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`Widget::size` is declared in this file: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    assert!(
        member.fact.range.start_offset < at(source, "void f"),
        "the jump goes to the member's declaration, not to the use"
    );
}

#[test]
fn a_member_of_another_class_with_the_same_name_is_not_the_answer() {
    // The wrong answer this query exists to avoid: two classes with a `size`, and the object decides which
    // one. A lookup that ignored the object would answer with whichever was declared first.
    let source = "struct Other {\n  int size;\n};\nstruct Widget {\n  int size;\n};\n\
                  void f() {\n  Widget w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`Widget::size` is the one the object names: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
    assert!(
        member.fact.range.start_offset > at(source, "struct Widget"),
        "the jump lands in `Widget`, not in `Other`"
    );
}

#[test]
fn a_member_access_reads_a_pointer_the_same_way() {
    // `ptr->member` names a member of the pointee. The type spelling of `Widget* p` is `Widget*`, and the
    // pointer is a fact about the declarator rather than about which class the name refers to.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the pointee's member is the answer: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
}

#[test]
fn a_member_access_reaches_a_class_in_an_included_header() {
    // The everyday case: the class is in the header, the object is local to the `.cpp`.
    let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
    let (index, tree) = analysed(
        &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::member_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "size = 1;"),
    );

    let Known::Yes(member) = found else {
        panic!("the member declared in the header is the answer: {found:?}");
    };
    assert_eq!(member.file, Path::new("/p/widget.h"));
    assert_eq!(member.fact.name, "size");
}

#[test]
fn a_member_access_through_a_qualified_type_resolves() {
    // `ns::Widget` as a type: the type spelling goes through the qualified-name machinery rather than being
    // treated as a plain name, which is what makes the two features compose instead of each needing its own
    // path.
    let source = "namespace ns {\n  struct Widget {\n    int size;\n  };\n}\n\
                  void f() {\n  ns::Widget w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`ns::Widget::size` is in this file: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("ns::Widget"));
}

#[test]
fn a_member_of_a_class_that_does_not_have_it_is_not_declared_here() {
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.nope = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "the class is known and the member is not in it: {found:?}"
    );
}

#[test]
fn a_member_access_on_a_call_is_the_callees_return_type() {
    // `make().size` — the construct `DeclFact::returns` exists for.
    //
    // This test used to assert the *opposite*, and its comment said so: "the boundary of this layer, stated as
    // an answer rather than as a wrong guess: `f().size` has a type, and working it out is the larger problem
    // the type layer will have to take on". That problem is now solved for a call whose callee is a
    // declaration this analysis can find, so the boundary moved — the failure of this test is what said so.
    let source = "struct Widget {\n  int size;\n};\nWidget make();\n\
                  void f() {\n  make().size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(definition) = found else {
        panic!("`make()` returns a `Widget`, so `size` is its member: {found:?}");
    };
    assert_eq!(definition.fact.name, "size");
    assert_eq!(definition.fact.type_of.as_deref(), Some("int"));

    // The other reading of the same tokens: `Widget()` is a **temporary** of the class rather than a call of
    // anything, and the declaration is what says which one this is.
    let constructed = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget().size = 1;\n}\n",
        "size = 1;",
    );
    assert!(
        matches!(constructed, Known::Yes(_)),
        "a temporary of a known class has the class's members: {constructed:?}"
    );
}

#[test]
fn a_member_access_through_a_dereference_resolves() {
    // **`(*make()).size` and `(*p).size`** — the dereference, which is arithmetic on a spelling rather than a
    // lookup: the pointer's declaration already says what it points at.
    //
    // This test asserted the *opposite* until the dereference was implemented, and its comment said so: "the
    // object is a dereference, not a call … a dereference, a subscript and an arithmetic expression each need
    // a type *computed* rather than read off a declaration". Two of the three are now computed, so the
    // boundary moved — the failure of this test is what said so, which is the way this file records a
    // capability landing.
    let through_a_call = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nWidget* make();\nvoid f() {\n  (*make()).size = 1;\n}\n",
        "size = 1;",
    );
    let Known::Yes(found) = through_a_call else {
        panic!("`make()` returns a `Widget*`, so `(*make())` is a `Widget`: {through_a_call:?}");
    };
    assert_eq!(found.fact.name, "size");

    // …and the same spelling through a declaration in the file: `Widget* p;`.
    let through_a_pointer = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  (*p).size = 1;\n}\n",
        "size = 1;",
    );
    let Known::Yes(found) = through_a_pointer else {
        panic!("`p` points at a `Widget`: {through_a_pointer:?}");
    };
    assert_eq!(found.fact.name, "size");

    // …and the same spelling through a **parameter**, whose declarator is written inside the parameter list
    // rather than at the top of a declaration: `void f(Widget* p)`.
    let through_a_parameter = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nvoid f(Widget* p) {\n  (*p).size = 1;\n}\n",
        "size = 1;",
    );
    let Known::Yes(found) = through_a_parameter else {
        panic!("`p` points at a `Widget`: {through_a_parameter:?}");
    };
    assert_eq!(found.fact.name, "size");

    // A reference is the other operator that says "the object is elsewhere", and `*` on it gives the same
    // answer — the declaration spells the type the same way up to the operator.
    let through_a_reference = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget& r = w;\n  (&r)->size = 1;\n}\n",
        "size = 1;",
    );
    assert!(
        matches!(through_a_reference, Known::Yes(_)),
        "`&r` is a `Widget*`: {through_a_reference:?}"
    );
}

#[test]
fn a_dereference_of_something_that_is_not_a_pointer_is_an_unknown_type() {
    // The boundary that is left, and it is a *refusal* rather than a wrong answer: `int* q` dereferenced is an
    // `int`, and an `int` is not a class — so there is nothing to look a member up in, and the answer says which
    // type it was that had no members. It used to say "not declared here" (the lookup went looking for a class
    // called `int`), which reads like a missing file rather than a wrong question. What must not happen, either
    // way, is an invented type.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  int* q;\n  (*q).size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
        "an `int` is not a class, and saying so beats inventing one: {found:?}"
    );

    // The other refusal: a subscript on a *class* is `operator[]`, and reaching its element type means
    // instantiating the template. Nothing about the spelling says which argument that is — `vector`'s is the
    // first and `map`'s is the second — so this answers `Unknown` rather than guessing.
    let vector = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\ntemplate<typename T> struct Box { T first; };\n\
         void f() {\n  Box<Widget> box;\n  box[0].size = 1;\n}\n",
        "size = 1;",
    );
    assert!(
        matches!(vector, Known::Unknown(UnknownReason::UnknownType(_))),
        "a class subscript needs the template instantiated: {vector:?}"
    );
}

#[test]
fn a_member_access_through_a_subscript_of_an_array_resolves() {
    // `arr[0].size` — an array's element type is written in the declaration: `Widget[4]` is four `Widget`s,
    // and the subscript takes the last `[…]` off.
    let source =
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget arr[4];\n  arr[0].size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(found) = found else {
        panic!("`arr[0]` is a `Widget`: {found:?}");
    };
    assert_eq!(found.fact.name, "size");

    // An index that is not a literal is the same question, and so is the second dimension of an array of
    // arrays: the subscript takes *one* pair of brackets off, outermost last.
    let two_dimensional = member_of(
        &[],
        "/p/a.cpp",
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget grid[2][3];\n  grid[i][j].size = 1;\n}\n",
        "size = 1;",
    );
    assert!(
        matches!(two_dimensional, Known::Yes(_)),
        "`grid[i][j]` is a `Widget`: {two_dimensional:?}"
    );
}

#[test]
fn a_call_of_something_with_no_return_type_is_an_unknown_type() {
    // **A deduced return the syntax can name is recorded.** `return Widget{}` is a prvalue of `Widget`, so
    // `make().size` resolves — the boundary moved here when the construction arm was added (fig 5: the hover had no
    // return type and the caller had nothing to substitute into).
    let source = "struct Widget {\n  int size;\n};\nauto make() { return Widget{}; }\n\
                  void f() {\n  make().size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");
    let Known::Yes(found) = found else {
        panic!("`return Widget{{}}` states the type it returns: {found:?}");
    };
    assert_eq!(found.fact.name, "size");

    // **A return that is a call is typed from the call's own declaration** — the boundary moved here with
    // `the_return_the_body_gives_back`, which types the body's `return` expression at query time, where the
    // declaring file's tree and scopes are in reach. `source()` returns `Widget`, so `make()` does too.
    let called = "struct Widget {\n  int size;\n};\nWidget source();\n\
                  auto make() { return source(); }\nvoid f() {\n  make().size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", called, "size = 1;");
    assert!(
        matches!(found, Known::Yes(_)),
        "`source()` is a `Widget`, so `make().size` names the member: {found:?}"
    );

    // **And the refusals that remain are the honest ones**: a declaration with no body at all, where there is no
    // `return` expression to read anywhere.
    let bodiless =
        "struct Widget {\n  int size;\n};\nauto make();\nvoid f() {\n  make().size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", bodiless, "size = 1;");
    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
        "the file does not state what this call has: {found:?}"
    );
}

#[test]
fn a_member_access_on_an_unknown_object_is_an_unknown_type() {
    // The object is a name, but nothing declares it — so there is no type to read, and the answer has to say
    // *that* rather than "the member is missing".
    let source = "void f() {\n  widget.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
        "nothing says what `widget` is: {found:?}"
    );
}

#[test]
fn a_cursor_that_is_not_on_a_member_access_has_nothing_to_look_up() {
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "= 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
        "the cursor is not on a member name: {found:?}"
    );
}

#[test]
fn a_nested_member_access_follows_the_types_down() {
    // This test used to assert the opposite — that `a.b.size` is `Unknown`, because inferring a nested
    // object's type was not done. It is done now, and this is what it bought: the inner access is typed by the
    // same query, so the outer one is answered by asking the inner one what it is.
    let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                  void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`Inner::size` is what `a.b.size` names: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
    assert!(
        member.fact.range.start_offset < at(source, "struct Outer"),
        "the jump lands in `Inner`, not in `Outer`"
    );
}

#[test]
fn a_nested_member_access_that_disagrees_with_the_outer_class_is_not_answered_by_it() {
    // `Outer` has a `size` of its own *and* a member whose type has one: the answer is the inner one, because
    // the object is `a.b`. A lookup that fell back to the outer class when the object was not a plain name
    // would land on `Outer::size`, which is the wrong entity and looks plausible.
    let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n  int size;\n};\n\
                  void f() {\n  Outer a;\n  a.b.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the member of `a.b`'s type is the answer: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Inner"));
}

#[test]
fn this_resolves_to_the_enclosing_class() {
    // `this` needs no inference at all: the scope chain already knows which class it is, which is why it is
    // the cheapest case in the type layer and the first one to handle.
    let source = "struct Widget {\n  int size;\n  void grow() {\n    this->size = 1;\n  }\n};\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`this->size` is `Widget::size`: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(member.fact.scope.as_deref(), Some("Widget"));
}

#[test]
fn this_resolves_to_the_nearest_class_when_classes_are_nested() {
    let source = "struct Outer {\n  int size;\n  struct Inner {\n    int size;\n    void f() {\n      \
                  this->size = 1;\n    }\n  };\n};\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`this` inside `Inner` is `Inner`: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Outer::Inner"));
}

#[test]
fn a_nested_access_propagates_why_the_inner_one_failed() {
    // The inner access is where the trouble is, and its reason is kept rather than flattened: there is no
    // member `nope` in `Outer`, so the inner one has no type — and saying "`Outer` has no `nope`" tells a
    // consumer what to fix, where "the type of `a.nope` is unknown" would only say that something is wrong.
    // (This test's first expectation was `UnknownType`, which is what flattening would produce.)
    let source = "struct Inner {\n  int size;\n};\nstruct Outer {\n  Inner b;\n};\n\
                  void f() {\n  Outer a;\n  a.nope.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "there is no `nope` to type, and that is the answer worth keeping: {found:?}"
    );
}

#[test]
fn a_member_inherited_from_a_base_class_resolves() {
    // The everyday OO case: the member is not in the class the object's type names, it is in the class that
    // one inherits from. Without the base walk this is `Unknown(NotDeclaredHere)`, which is the answer a user
    // would see for most of their code.
    let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int other;\n};\n\
                  void f() {\n  Derived d;\n  d.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("`Base::size` is inherited by `Derived`: {found:?}");
    };
    assert_eq!(member.fact.name, "size");
    assert_eq!(member.fact.scope.as_deref(), Some("Base"));
    assert!(
        member.fact.range.start_offset < at(source, "struct Derived"),
        "the jump lands in the base, not in the derived class"
    );
}

#[test]
fn a_member_of_the_class_itself_hides_one_in_a_base() {
    // Level by level, and the class's own level is first: a `size` written in `Derived` wins over `Base`'s,
    // which is what the language does and what a reader expects.
    let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int size;\n};\n\
                  void f() {\n  Derived d;\n  d.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the class's own member hides the one it inherits: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Derived"));
}

#[test]
fn a_member_inherited_through_another_base_resolves() {
    // Two levels: `Derived` has nothing, `Middle` has nothing, `Base` has it.
    let source = "struct Base {\n  int size;\n};\nstruct Middle : public Base {\n  int m;\n};\n\
                  struct Derived : public Middle {\n  int d;\n};\n\
                  void f() {\n  Derived x;\n  x.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("a member two bases down is still inherited: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Base"));
}

#[test]
fn a_member_declared_in_two_bases_at_once_is_ambiguous_rather_than_picked() {
    // A diamond without `virtual`: both sides declare `size`, so the name is not uniquely resolved by the
    // language. Picking the first would be a jump to an entity the user cannot tell from the other, which is
    // exactly the wrong answer this project refuses to give.
    let source = "struct Left {\n  int size;\n};\nstruct Right {\n  int size;\n};\n\
                  struct Both : public Left, public Right {\n  int own;\n};\n\
                  void f() {\n  Both b;\n  b.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
        "two bases declare it and nothing chooses: {found:?}"
    );
}

#[test]
fn a_member_inherited_across_a_header_resolves() {
    // The base is in the header and the derived class is in the file being edited: the base *list* comes from
    // this file's tree and the base *class* from the index, which is the two halves meeting.
    let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n\
                  void f() {\n  Derived x;\n  x.size = 1;\n}\n";
    let (index, tree) = analysed(
        &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::member_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "size = 1;"),
    );

    let Known::Yes(member) = found else {
        panic!("the inherited member is declared in the header: {found:?}");
    };
    assert_eq!(member.file, Path::new("/p/base.h"));
    assert_eq!(member.fact.name, "size");
}

#[test]
fn a_member_nobody_declares_is_still_not_declared() {
    // The base walk must not turn "not found" into "found somewhere": the honest answer survives it.
    let source = "struct Base {\n  int size;\n};\nstruct Derived : public Base {\n  int d;\n};\n\
                  void f() {\n  Derived x;\n  x.nope = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "nope = 1;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "neither the class nor its base declares it: {found:?}"
    );
}

#[test]
fn a_base_list_is_read_without_its_access_keywords() {
    // `public Base` is one base called `Base`: reading the whole specifier would look for a class named
    // "public Base", which is a name nothing declares.
    let source = "struct Base {\n  int size;\n};\nstruct Derived : private virtual Base {\n  int d;\n};\n\
                  void f() {\n  Derived x;\n  x.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("the access and `virtual` are not part of the base's name: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("Base"));
}

#[test]
fn a_base_that_inherits_from_itself_terminates() {
    // A cycle in the base list cannot be written in valid C++, and a malformed file can produce one: the
    // visited set is what keeps the walk from being an infinite loop on a file that is being typed.
    let source = "struct A : public B {\n  int a;\n};\nstruct B : public A {\n  int size;\n};\n\
                  void f() {\n  A x;\n  x.size = 1;\n}\n";
    let found = member_of(&[], "/p/a.cpp", source, "size = 1;");

    let Known::Yes(member) = found else {
        panic!("a cycle must not stop the member being found: {found:?}");
    };
    assert_eq!(member.fact.scope.as_deref(), Some("B"));
}

#[test]
fn a_local_declaration_still_wins_when_the_header_also_has_the_name() {
    // `count` is declared both here and in the header. The answer is this file's, and it is reached without
    // the index — which is the resolution order, not an optimisation.
    let source = "#include \"widget.h\"\nint count = 0;\nvoid f() {\n  count = 1;\n}\n";
    let (index, tree) = analysed(
        &[("/p/widget.h", "int count = 7;\n")],
        "/p/main.cpp",
        source,
    );

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "count = 1;"),
    );

    let Known::Yes(definition) = found else {
        panic!("the file's own declaration wins: {found:?}");
    };
    assert_eq!(definition.file, Path::new("/p/main.cpp"));
}

// -------------------------------------------------------------------------------------------
// Member lists
//
// The *list* form of the member query, and the fixtures are written to make the one decision it makes
// visible: which class declares each member, and whether that is the type asked about or a base. The
// staleness test at the end is the reason none of it is stored.
// -------------------------------------------------------------------------------------------

/// The members of `class`, with the file analysed and indexed the way a real query has them.
fn members_of_class(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
    class: &str,
) -> Known<super::MemberList> {
    let (index, tree) = analysed(files, from, source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    super::members_of(&index, &scopes, &root, Path::new(from), class)
}

/// The member names, in the order the query produced them.
fn names(list: &super::MemberList) -> Vec<&str> {
    list.members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect()
}

/// **An anonymous union's members are the enclosing struct's members** — `line.integer` in a C corpus.
///
/// ```c
/// struct line {
///     struct point from;
///     union { long integer; double real; };   // no member name between the struct and the union
///     char colour;
/// };
/// ```
///
/// C11's anonymous members put `integer` and `real` in `line`'s member space. The scope walk reached the
/// declaration, saw one with no name, walked its body — and the body's declarations landed in a scope of their own
/// instead of in `line`, so a completion after `line.` offered four of the six names.
#[test]
fn an_anonymous_unions_members_belong_to_the_struct() {
    let source = "struct point { int x; int y; };\n\
struct line {\n\
struct point from;\n\
union { long integer; double real; };\n\
char colour;\n\
};\n";
    let found = members_of_class(&[], "/p/a.c", source, "line");
    let Known::Yes(list) = found else {
        panic!("`line` is declared here: {found:?}");
    };
    assert!(
        names(&list).contains(&"integer"),
        "the union is anonymous, so its members are `line`'s: {:?}",
        names(&list)
    );
    assert!(
        names(&list).contains(&"real"),
        "and so is `real`: {:?}",
        names(&list)
    );
}

/// **A C struct declared in a header has its members for a file that includes it.** The C23 `_BitInt` specifier is
/// part of a declaration, not a reason to drop the declaration: the corpus project's four members are recorded by
/// the header's own summary and must be visible from the source that includes it.
#[test]
fn a_c_struct_in_a_header_has_its_members() {
    let header = "struct bits {\n  unsigned _BitInt(3) three : 3;\n  _BitInt(13) signed_wide : 13;\n  _BitInt(5) plain;\n  unsigned _BitInt(4) other : 2;\n};\n";
    let source = "#include \"corpus.h\"\nstruct bits make_bits(unsigned seed) {\n  struct bits value = {0};\n  value.three = seed;\n  return value;\n}\n";
    let found = members_of_class(&[("/p/corpus.h", header)], "/p/bits.c", source, "bits");

    let Known::Yes(list) = found else {
        panic!("`bits` is declared in the header this file includes: {found:?}");
    };
    let mut found = names(&list);
    found.sort_unstable();
    assert_eq!(
        found,
        ["other", "plain", "signed_wide", "three"],
        "every member the header writes is in the list: {:?}",
        names(&list)
    );
}

#[test]
fn an_elaborated_type_specifier_does_not_declare_the_tag() {
    // `struct bits make_bits(...)` is the *use* the corpus project's `bits.c` opens with, and binding the tag there
    // created a type fact in that file with no members. A jump on the parameter's `bits` then resolved to it —
    // reported as landing on `make_bits` — instead of the definition the header writes.
    let header = "struct bits {\n  unsigned _BitInt(3) three : 3;\n};\n";
    let source = "#include \"corpus.h\"\nstruct bits make_bits(unsigned seed) {\n  struct bits value = {0};\n  return value;\n}\n";
    let (index, tree) = analysed(&[("/p/corpus.h", header)], "/p/bits.c", source);

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/bits.c"),
        at(source, "struct bits make_bits") + 7,
    );

    let Known::Yes(definition) = found else {
        panic!("`struct bits` names the header's type: {found:?}");
    };
    assert_eq!(
        definition.file,
        Path::new("/p/corpus.h"),
        "the jump goes to the definition, not to this file's `make_bits`"
    );
    assert_eq!(definition.fact.name, "bits");
}

#[test]
fn an_elaborated_enum_specifier_does_not_declare_the_tag() {
    // The class rule, one kind over: `enum Color pick(void);` is a use of `Color`, and binding it in this file made
    // the jump land on the function's own line instead of the enumeration the header writes.
    let header = "enum Color { Red, Green };\n";
    let source = "#include \"corpus.h\"\nenum Color pick(void) {\n  return Red;\n}\n";
    let (index, tree) = analysed(&[("/p/corpus.h", header)], "/p/bits.c", source);

    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);
    let found = super::definition_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/bits.c"),
        at(source, "enum Color pick") + 5,
    );

    let Known::Yes(definition) = found else {
        panic!("`enum Color` names the header's enumeration: {found:?}");
    };
    assert_eq!(
        definition.file,
        Path::new("/p/corpus.h"),
        "the jump goes to the definition, not to this file's `pick`"
    );
    assert_eq!(definition.fact.name, "Color");
}

#[test]
fn a_class_declared_more_than_once_still_reports_the_bases_it_could_not_read() {
    // MSVC's `<istream>`, in miniature: the class, and an explicit instantiation of it. Measured on the real
    // header, `std::basic_istream` is declared three times — the class and two
    // `template class _CRTIMP2_PURE_IMPORT basic_istream<char, …>;` lines — and the inherited members used
    // to be missing with `unlisted` **empty**, which is a list claiming a completeness it does not have.
    //
    // **What changed, and why the expectation is `Base` rather than `Stream`.** The base walk used to ask
    // `definition` for the class, which answers `Unknown(Ambiguous)` for a name declared twice, and an
    // ambiguous class answered *no base list at all* — so the base clause was never even read, and the
    // class itself was named as the thing that could not be read. It now merges the bases of **every**
    // declaration, which is what makes `xmemory`'s `allocator_traits` — forward-declared at 554 with no
    // base and defined at 755 with one — inherit anything at all. What is left unread is the base the
    // clause actually names.
    //
    // `Base` is named here because `a.cpp` does not include the header that declares it, which is the
    // honest gap and the one this field exists for: the list says which name it could not follow rather
    // than quietly ending.
    let source = "template <class E>\nstruct Stream : Base {\n  int read;\n};\ntemplate struct Stream<char>;\n";
    let found = members_of_class(
        &[("/p/base.h", "struct Base { int eof; };\n")],
        "/p/a.cpp",
        source,
        "Stream",
    );

    let Known::Yes(list) = found else {
        panic!("`Stream` is declared here: {found:?}");
    };
    assert_eq!(names(&list), ["read"], "its own member is listed");
    assert_eq!(
        list.unlisted
            .iter()
            .map(|base| base.spelling.as_str())
            .collect::<Vec<_>>(),
        ["Base"],
        "and the base the walk could not follow is named, so the list does not claim completeness"
    );
}

#[test]
fn a_class_lists_the_members_written_in_it() {
    // Written out of alphabetical order on purpose: a member list is ordered by name rather than by the text,
    // because the file's scope tree keeps its bindings name-sorted while the index keeps facts in offset
    // order, and the two have to agree for a class to list the same way wherever it is declared.
    let source = "struct Widget {\n  void grow();\n  int size;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

    let Known::Yes(list) = found else {
        panic!("`Widget` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["grow", "size"]);
    assert!(
        list.unlisted.is_empty(),
        "a class with no bases has nothing left unread: {:?}",
        list.unlisted
    );

    let size = &list.members[1];
    assert_eq!(size.declared_in, "Widget");
    assert_eq!(size.depth, 0, "its own member, not an inherited one");
    assert!(!size.ambiguous);
    assert_eq!(size.file, Path::new("/p/a.cpp"));
    assert_eq!(
        size.fact.type_of.as_deref(),
        Some("int"),
        "the type a completion shows beside the name"
    );
    assert_eq!(
        list.own().count(),
        2,
        "every member here is the class's own"
    );
}

#[test]
fn a_derived_class_lists_its_own_members_before_the_ones_it_inherits() {
    // Nearest first, which is the order C++ hides in — and the base is tagged rather than flattened into the
    // derived class, so a consumer can show an inherited member as inherited and can jump to `Base::count`
    // rather than to a copy of it.
    let source = "struct Base {\n  int inherited_count;\n};\n\
                  struct Derived : public Base {\n  int own_count;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("`Derived` is declared in this file: {found:?}");
    };

    assert_eq!(
        names(&list),
        ["own_count", "inherited_count"],
        "the class's own members come first, and the inherited one is last"
    );
    assert_eq!(list.members[0].declared_in, "Derived");
    assert_eq!(list.members[0].depth, 0);
    assert_eq!(
        list.members[1].declared_in, "Base",
        "the member is the base's, not the derived class's"
    );
    assert_eq!(list.members[1].depth, 1);
    assert_eq!(
        list.members[1].fact.type_of.as_deref(),
        Some("int"),
        "and it carries its own declaration's type"
    );
    assert_eq!(list.at_depth(1).count(), 1);
}

#[test]
fn a_member_the_class_redeclares_hides_the_one_in_its_base() {
    // Hiding is by name, not by signature: `Derived::size` hides `Base::size` whatever the parameters are,
    // so the base's is not in the list. A list that kept both would offer a name the language does not find.
    let source = "struct Base {\n  int size;\n};\n\
                  struct Derived : public Base {\n  double size;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("`Derived` is declared in this file: {found:?}");
    };

    assert_eq!(
        names(&list),
        ["size"],
        "one `size`, and it is the derived class's"
    );
    assert_eq!(list.members[0].declared_in, "Derived");
    assert_eq!(list.members[0].fact.type_of.as_deref(), Some("double"));
}

#[test]
fn a_name_two_bases_declare_is_listed_twice_and_marked_ambiguous() {
    // The list form of the ambiguity the single-member query reports: both declarations are kept, because
    // dropping either would be choosing one for the user, and both are flagged, because a consumer offering
    // the name has to be able to say that the language does not resolve it here.
    let source = "struct Left {\n  int value;\n};\nstruct Right {\n  int value;\n};\n\
                  struct Both : public Left, public Right {\n  int own;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Both");

    let Known::Yes(list) = found else {
        panic!("`Both` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["own", "value", "value"]);

    let values: Vec<&super::ProjectMember> = list
        .members
        .iter()
        .filter(|member| member.fact.name == "value")
        .collect();
    assert_eq!(values.len(), 2, "both declarations survive into the list");
    assert!(
        values.iter().all(|member| member.ambiguous),
        "both are contested: {values:?}"
    );

    let mut declaring: Vec<&str> = values
        .iter()
        .map(|member| member.declared_in.as_str())
        .collect();
    declaring.sort_unstable();
    assert_eq!(declaring, ["Left", "Right"]);

    assert!(
        !list.members[0].ambiguous,
        "`own` is declared once, in one class"
    );
}

#[test]
fn a_destructor_hides_nothing_in_a_base_because_its_spelling_is_its_own() {
    // `virtual ~Base();` — the name the binding holds is `~Base`, and a destructor's `identifier_text()` is
    // `None`, so this path used to store an **empty** name. Comparing two empty names made `~Derived` hide
    // `~Base`, which is a wrong answer arrived at by treating a missing spelling as if it were a spelling.
    //
    // The spelling is stored now (`DeclFact::name` is `binding.name.text()`, the same one the index files), so the
    // two destructors are two *different* names and neither hides the other for the right reason rather than by
    // exception — and an operator, which had the same empty name, is a member a lookup can find.
    let source = "struct Base {\n  virtual ~Base();\n  int size;\n};\n\
                  struct Derived : public Base {\n  virtual ~Derived();\n  int d;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("`Derived` is declared in this file: {found:?}");
    };

    let destructors: Vec<&super::ProjectMember> = list
        .members
        .iter()
        .filter(|member| member.fact.name.starts_with('~'))
        .collect();
    assert_eq!(
        destructors.len(),
        2,
        "both destructors are declarations that exist, and dropping one would be a silent omission: {:?}",
        names(&list)
    );

    let mut classes: Vec<&str> = destructors
        .iter()
        .map(|member| member.declared_in.as_str())
        .collect();
    classes.sort_unstable();
    assert_eq!(classes, ["Base", "Derived"]);

    let mut spellings: Vec<&str> = destructors
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    spellings.sort_unstable();
    assert_eq!(
        spellings,
        ["~Base", "~Derived"],
        "a destructor's name is the class it destroys, so the two are not one name"
    );

    assert!(
        destructors.iter().all(|member| !member.ambiguous),
        "two different spellings are not one contested name: {destructors:?}"
    );
    assert!(
        list.members
            .iter()
            .any(|member| member.fact.name == "size" && member.declared_in == "Base"),
        "and the named members still cross the inheritance boundary: {:?}",
        names(&list)
    );
}

/// **A destructor written without a specifier is a member, and is listed by its spelling.**
///
/// Two halves of this were broken at different times, and the fixture pins both.
///
/// The first is the *class's* name. A constructor and a destructor have no `decl-specifier-seq`, so the grammar
/// puts their name straight under the declaration, and `name_from_text` used to read the class's name off the
/// destructor inside its body: `Derived` was never bound and `d` was filed under `~Derived`. That half is green
/// (`tests/scopes.rs`, shape `File{Widget}(Class{size,~Widget})`).
///
/// The second is the *fact's* name, and it is why this test was `#[ignore]`d for several rounds with the
/// expectation `["~Derived", "d"]` written down: `DeclFact::name` was empty for anything without an identifier —
/// a destructor, an operator, a conversion function — on the argument that their spellings are not lookup keys.
/// Measured, that cost more than it bought: `members_of` answered `[""]` for a class whose member is
/// `operator[]`, so an operator was invisible to every member query, and a destructor answered for its own class
/// in `definition`. A fact now carries the **spelling** (`binding.name.text()`), so the member list reads
/// `~Derived` — the round that fixed it is the one that also removed the `#[ignore]`.
///
/// The order is the list's, not the source's: members are sorted by name, so `d` comes before `~Derived`.
#[test]
fn a_destructor_without_a_specifier_is_a_member() {
    let source = "struct Derived {\n  ~Derived();\n  int d;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("the class must be found by its own name: {found:?}");
    };
    assert_eq!(names(&list), ["d", "~Derived"]);
}

#[test]
fn overloads_in_one_class_are_not_ambiguous() {
    // The other half of the rule above, and the case a count-of-declarations implementation gets wrong:
    // `f` is declared twice and is one name in one class, so nothing about it is contested.
    let source = "struct Widget {\n  void f();\n  void f(int);\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Widget");

    let Known::Yes(list) = found else {
        panic!("`Widget` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["f", "f"], "both overloads are listed");
    assert!(
        list.members.iter().all(|member| !member.ambiguous),
        "one class declaring a name twice is an overload set, not an ambiguity: {:?}",
        list.members
    );
}

#[test]
fn a_cycle_of_bases_terminates_and_each_class_contributes_once() {
    // A base list that cannot be written in valid C++ and can be produced by a malformed file. The visited set
    // is what keeps the walk finite; without it this test does not fail, it does not return.
    let source =
        "struct A : public B {\n  int a_member;\n};\nstruct B : public A {\n  int b_member;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "A");

    let Known::Yes(list) = found else {
        panic!("`A` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["a_member", "b_member"]);
    assert_eq!(list.members[0].depth, 0);
    assert_eq!(
        list.members[1].depth, 1,
        "`A` is reached again as `B`'s base and is not listed a second time"
    );
}

#[test]
fn a_template_class_lists_the_members_it_was_written_with() {
    // No instantiation, and the answer says so by what it contains: `value` has the type `T`, which is what
    // the class wrote. Instantiating would mean picking an argument, and a member list for `Holder<int>` and
    // `Holder<std::string>` would then be two different lists — which is a `sema` question, not this one.
    let source = "template <typename T>\nstruct Holder {\n  T value;\n  int count;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Holder");

    let Known::Yes(list) = found else {
        panic!("`Holder` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["count", "value"]);
    assert_eq!(
        list.members[1].fact.type_of.as_deref(),
        Some("T"),
        "the parameter as written, not an instantiated type"
    );
    assert!(
        !list.members.iter().any(|member| member.fact.name == "T"),
        "the template parameter is declared in the parameter list, not in the class: {:?}",
        names(&list)
    );
}

#[test]
fn a_type_nothing_declares_has_no_member_list_rather_than_an_empty_one() {
    // The distinction the whole crate is built around, asked of a list: an empty list is a claim that the type
    // has no members, and this analysis cannot make it about a type it has never seen.
    let source = "void f() { }\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Nowhere");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "`Nowhere` is not a type this analysis has seen: {found:?}"
    );
}

#[test]
fn a_class_that_is_empty_has_an_empty_member_list() {
    // The other side of the test above, and the one that needs the *name* to be asked about rather than the
    // members: `struct Empty { };` writes no member and no scope either — see `scopes::class_like` — so a
    // query that read "no members found" as "no class found" would report an empty class as a missing one.
    let source = "struct Empty { };\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Empty");

    let Known::Yes(list) = found else {
        panic!("`Empty` is declared in this file and has no members: {found:?}");
    };
    assert!(list.members.is_empty());
}

#[test]
fn a_member_declared_in_a_nested_class_is_not_a_member_of_the_outer_one() {
    // The scope a fact records is the scope it was written in, so `Inner::x` is a member of `Inner` and the
    // member of `Outer` is the class `Inner`. A list built by name rather than by scope would put `x` in both.
    let source = "struct Outer {\n  struct Inner {\n    int x;\n  };\n  int y;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Outer");

    let Known::Yes(list) = found else {
        panic!("`Outer` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["Inner", "y"]);
    assert_eq!(list.members[0].fact.qualified_name(), "Outer::Inner");
}

#[test]
fn a_member_list_crosses_a_header_and_keeps_the_base_tagged() {
    // The everyday layout: the base is in a header, the derived class is in the file being edited. The base
    // list comes from the buffer's tree and the base's members from the index, and the answer has to name the
    // header as the place to jump to rather than the buffer.
    let source = "#include \"base.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
    let (index, tree) = analysed(
        &[("/p/base.h", "struct Base {\n  int size;\n};\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let found = super::members_of(&index, &scopes, &root, Path::new("/p/main.cpp"), "Derived");
    let Known::Yes(list) = found else {
        panic!("`Derived` is declared in the buffer: {found:?}");
    };

    assert_eq!(names(&list), ["d", "size"]);
    assert_eq!(list.members[1].file, Path::new("/p/base.h"));
    assert_eq!(list.members[1].declared_in, "Base");
}

#[test]
fn a_base_that_resolves_to_a_template_is_listed_without_instantiating_it() {
    // `public Base<int>` is the class `Base` for the purpose of finding members: the arguments say which type
    // is inherited, not which class declares what. Reading the spelling literally would look for a class named
    // `Base<int>`, which is a name no declaration has.
    //
    // **What the arguments are kept for is the member, not the lookup.** A member inherited from a template base is
    // written in that base's parameters (`T size;`), and substituting them is what makes its type the one a reader of
    // `Derived` sees. The tag therefore names the instantiation: `Base<int>` is where the member came from.
    let source = "template <typename T>\nstruct Base {\n  T size;\n};\n\
                  struct Derived : public Base<int> {\n  int d;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("`Derived` is declared in this file: {found:?}");
    };

    assert_eq!(names(&list), ["d", "size"]);
    assert_eq!(
        list.members[1].fact.type_of.as_deref(),
        Some("int"),
        "the base's parameter is substituted: {list:?}"
    );
    assert_eq!(list.members[1].declared_in, "Base<int>");
}

#[test]
fn a_base_nothing_declares_is_reported_as_a_gap_rather_than_dropped() {
    // The list is incomplete and says so. Answering with just `d` would be a claim that `Derived` has one
    // member, which is exactly what this analysis cannot know: `Base` is behind an include nobody indexed.
    let source = "#include \"missing.h\"\nstruct Derived : public Base {\n  int d;\n};\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "Derived");

    let Known::Yes(list) = found else {
        panic!("`Derived` itself is in the buffer: {found:?}");
    };

    assert_eq!(names(&list), ["d"], "what could be listed is listed");
    assert_eq!(list.unlisted.len(), 1, "and what could not is named");
    assert_eq!(list.unlisted[0].spelling, "Base");
    assert!(
        matches!(list.unlisted[0].reason, UnknownReason::NotDeclaredHere(_)),
        "the reason has to say that the base is not here, not that the list ended: {:?}",
        list.unlisted[0].reason
    );
}

#[test]
fn a_base_of_a_class_in_a_namespace_is_looked_up_in_that_namespace() {
    // The rule that took `examples/std_query.rs` from 7/9 to 9/9, in the shape MSVC's STL has it: `std::map`
    // inherits from `_Tree`, `_Tree` is declared in `<xtree>` inside `_STD_BEGIN` (= `namespace std {`), and
    // the base clause spells it **unqualified**. A base name is looked up from the scope *enclosing* the class
    // and outward, so `_Tree` written inside `std` names `std::_Tree` — and the file-scope `_Tree` below,
    // which declares something else entirely, must not be consulted: the enclosing namespace is a nearer scope
    // and unqualified lookup stops at the first scope that has the name.
    //
    // Both walks are asserted here because both had the same hole and a fix to one is invisible from the
    // other: the list (`members_of`) and the single member (`member_across_files`).
    let lib = "namespace std {\nstruct _Tree {\n  void find();\n};\nstruct map : _Tree {\n  int count;\n};\n}\n";
    let source = "#include \"lib.h\"\nstruct _Tree {\n  void wrong();\n};\nvoid f() {\n  std::map m;\n  m.find();\n  m.wrong();\n}\n";
    let files: &[(&str, &str)] = &[("/p/lib.h", lib)];

    let Known::Yes(list) = members_of_class(files, "/p/a.cpp", source, "std::map") else {
        panic!("`std::map` is in the indexed header");
    };
    assert_eq!(
        names(&list),
        ["count", "find"],
        "`find` comes from the base, and `wrong` is not a member of anything this class inherits"
    );
    assert!(
        list.unlisted.is_empty(),
        "the base was found, so nothing here is a gap: {:?}",
        list.unlisted
    );
    assert_eq!(
        list.members[1].declared_in, "std::_Tree",
        "the answer names the scope the base was found in, not the spelling the class wrote"
    );
    assert_eq!(list.members[1].depth, 1);

    let found = member_of(files, "/p/a.cpp", source, "find();");
    let Known::Yes(found) = found else {
        panic!("`m.find` is `std::_Tree::find`: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/lib.h"));
    assert_eq!(found.fact.qualified_name(), "std::_Tree::find");

    assert!(
        matches!(
            member_of(files, "/p/a.cpp", source, "wrong();"),
            Known::Unknown(UnknownReason::NotDeclaredHere(_))
        ),
        "a base name that a nearer scope has does not fall through to the file-scope `_Tree`"
    );
}

#[test]
fn a_type_spelled_from_the_global_name_space_lists_the_same_members() {
    // `::Widget w;` is a declaration whose type spelling carries the leading `::`, and a consumer feeding this
    // query from `DeclFact.type_of` therefore hands it `::Widget`. The index keys a global declaration under
    // its bare name — a file-scope declaration has no scope prefix — so the prefix has to come off before the
    // walk, or a class that is plainly in the buffer answers `Unknown(NotDeclaredHere("::Widget"))`.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  ::Widget w;\n}\n";
    let found = members_of_class(&[], "/p/a.cpp", source, "::Widget");

    let Known::Yes(list) = found else {
        panic!("`::Widget` is the file-scope `Widget`: {found:?}");
    };
    assert_eq!(names(&list), ["size"]);
    assert_eq!(list.members[0].declared_in, "Widget");
}

#[test]
fn a_member_added_to_a_base_appears_without_reindexing_the_derived_class() {
    // The test that decides whether inherited members are *materialized*, and the reason they are not.
    //
    // Only `b.h` is rebuilt below. Nothing about `A`'s text or its key changes, so a summary that stored the
    // members `A` inherits would go on answering with `old_member` for ever — and no invalidation rule could
    // catch it, because there is nothing to invalidate: the edit is in a file `A` never mentions by name.
    // Walking the chain at query time is what makes the second assertion true.
    let a_header = "#include \"b.h\"\nstruct A : public B {\n  int a_member;\n};\n";
    let before = "struct B {\n  int old_member;\n};\n";
    let after = "struct B {\n  int new_member;\n};\n";
    let main = "#include \"a.h\"\n";

    let mut index = index(&[
        ("/p/main.cpp", main),
        ("/p/a.h", a_header),
        ("/p/b.h", before),
    ]);

    let tree = cpp_parser::CppParser::parse(main, cpp_parser::ParserConfig::default());
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let listed = |index: &ProjectIndex| match super::members_of(
        index,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        "A",
    ) {
        Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.to_string())
            .collect::<Vec<_>>(),
        other => panic!("`A` is declared in an included header: {other:?}"),
    };

    assert_eq!(listed(&index), ["a_member", "old_member"]);

    // `b.h` and nothing else. `a.h` keeps the summary it was built with, which is the whole point.
    let mut rebuilt = summarize(Path::new("/p/b.h"), after, SummaryKey::new(1, 0));
    for include in &mut rebuilt.includes {
        include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
    }
    index.insert(rebuilt);

    assert_eq!(
        listed(&index),
        ["a_member", "new_member"],
        "the derived class's members are read from its bases at query time, so the edit is seen without \
         anything of `A`'s being rebuilt"
    );

    // And why that worked, asserted rather than implied: `A`'s own summary holds its own member and nothing it
    // inherits. The day something starts writing inherited members into a derived class's summary, this is
    // where it shows up — before the stale answers do.
    let a = index
        .summary(Path::new("/p/a.h"))
        .expect("`a.h` is indexed");
    let a_members: Vec<&str> = a
        .declarations
        .iter()
        .filter(|fact| fact.scope.as_deref() == Some("A"))
        .map(|fact| fact.name.as_str())
        .collect();
    assert_eq!(
        a_members,
        ["a_member"],
        "a fact stores the spelling `B`, never the members `B` happens to have"
    );
}

// -------------------------------------------------------------------------------------------
// Completion: the cursor-to-members query
//
// Every fixture here is an **incomplete program**, which is the only honest way to test this: `w.` does not
// parse, and a test that made it parse would be testing a question nobody asks. The helper below therefore
// does not assert a clean parse — see `analysed_while_typing`.
// -------------------------------------------------------------------------------------------
/// The offset just **past** the last `needle`, which is where the cursor is after that much has been typed.
fn after(source: &str, needle: &str) -> usize {
    source
        .rfind(needle)
        .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
        + needle.len()
}

/// The members offered at the cursor just past `needle`.
fn completions_at(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
    needle: &str,
) -> Known<super::MemberCompletions> {
    let (index, tree) = analysed_while_typing(files, from, source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    super::member_completions_at(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new(from),
        after(source, needle),
    )
}

/// **A `private` member is not offered from outside the class**, and is offered from inside it.
///
/// The two directions in one fixture, because a rule that hid everything would pass the first half alone: what
/// the reader may *write* is the question, and C++ answers it by where the cursor is — see
/// [`DeclFact::access`] for the field and [`offerable_members`] for the rule.
///
/// The default access of a `class` is the other half: `int hidden;` written before any label is private, which
/// is why the key matters and not just the labels.
#[test]
fn a_private_member_is_offered_inside_the_class_and_not_outside_it() {
    let outside = "\
class Widget {
int hidden;
public:
int shown;
};
void f() {
Widget w;
w.
}
";
    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", outside, "w.") else {
        panic!("`w` is a `Widget`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert_eq!(
        offered,
        ["shown"],
        "`hidden` is private and the cursor is not in `Widget`"
    );
    assert_eq!(
        found.hidden_by_access, 1,
        "and the answer says how many it kept out"
    );

    let inside = "\
class Widget {
int hidden;
public:
int shown;
void grow(Widget& other) {
    other.
}
};
";
    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", inside, "other.") else {
        panic!("`other` is a `Widget`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert_eq!(
        offered,
        ["grow", "hidden", "shown"],
        "inside the class both are nameable, so both are offered"
    );
}

/// **A member defined outside its class is still that class's member** — for access, which is the question this
/// test is about, and for everything else that is a property of the declaration.
///
/// The shape is one every real C++ codebase is written in: the class states the declaration (and with it the
/// access) and the definition lives further down the file, in a section of its own with no access label in
/// sight. `declared_access_at` reads the level *where the name is written*, so at an out-of-class definition it
/// finds no class body at all — see the expectations below.
#[test]
fn a_member_defined_outside_its_class_keeps_its_access() {
    let source = "\
class Widget {
int hidden;
public:
int shown;
void set(int value);
void grow(Widget& other);
};

void Widget::set(int value) {
hidden = value;
}

void Widget::grow(Widget& other) {
other.
}

void f(Widget& w) {
w.
}
";
    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", source, "other.") else {
        panic!("`other` is a `Widget`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert!(
        offered.contains(&"hidden"),
        "the cursor is inside a member of `Widget`, so `hidden` is nameable there: {offered:?}"
    );
    assert!(
        offered.contains(&"set") && offered.contains(&"shown") && offered.contains(&"grow"),
        "and so is everything else the class declares: {offered:?}"
    );

    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", source, "w.") else {
        panic!("`w` is a `Widget`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert!(
        !offered.contains(&"hidden"),
        "and from outside the class the same member is not: {offered:?}"
    );
}

/// **A `protected` member is offered in a class derived from the one that declares it** — and not from a free
/// function, which is the whole difference between `protected` and `public`.
///
/// The base chain is walked from the cursor's own class ([`classes_the_cursor_is_in`]), which is what makes the
/// derived case work: the member's own class is not where the cursor is, but it is an ancestor of it.
#[test]
fn a_protected_member_is_offered_in_a_derived_class() {
    let source = "\
class Base {
protected:
int guarded;
public:
int open;
};
class Derived : public Base {
void grow(Derived& other) {
    other.
}
};
void f(Derived& d) {
d.
}
";
    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", source, "other.") else {
        panic!("`other` is a `Derived`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert_eq!(
        offered,
        ["grow", "guarded", "open"],
        "`guarded` is protected and the cursor is in a class derived from the one that declares it"
    );

    let Known::Yes(found) = completions_at(&[], "/p/a.cpp", source, "d.") else {
        panic!("`d` is a `Derived`");
    };
    let offered: Vec<&str> = found
        .members
        .members
        .iter()
        .map(|m| m.fact.name.as_str())
        .collect();
    assert_eq!(
        offered,
        ["open"],
        "and from a free function the same member is not nameable"
    );
}

#[test]
fn a_member_access_with_nothing_typed_offers_the_types_members() {
    // The keystroke that asks the question. `w.` is not a program and does not parse, which is exactly why
    // this is a query over a damaged tree rather than over a compiled one.
    let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                  void f() {\n  Widget w;\n  w.\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "w.");

    let Known::Yes(completions) = found else {
        panic!("`w` is a `Widget` declared two lines up: {found:?}");
    };

    assert_eq!(completions.class, "Widget");
    assert_eq!(
        completions
            .members
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>(),
        ["grow", "size"]
    );
    assert_eq!(completions.prefix, "", "nothing of the name is written yet");
    assert_eq!(
        completions.member_range.start_offset,
        after(source, "w."),
        "the empty range sits just past the operator, which is where the name goes"
    );
    assert_eq!(completions.member_range.length, 0);
}

#[test]
fn a_half_typed_member_offers_the_same_members_and_names_the_prefix() {
    // The other half of the same question: the list is the *whole* list — filtering is the client's job and
    // it has the document — while the range is what the client replaces with the chosen member.
    let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                  void f() {\n  Widget w;\n  w.si\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "w.si");

    let Known::Yes(completions) = found else {
        panic!("the cursor is on the member being typed: {found:?}");
    };

    assert_eq!(completions.class, "Widget");
    assert_eq!(completions.prefix, "si");
    assert_eq!(
        &source[completions.member_range.start_offset..completions.member_range.end_offset()],
        "si",
        "the range covers what is written, so replacing it does not leave a doubled name"
    );
}

#[test]
fn a_pointer_member_access_offers_the_pointees_members() {
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget* p;\n  p->\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "p->");

    let Known::Yes(completions) = found else {
        panic!("`Widget* p` is a pointer to a `Widget`: {found:?}");
    };
    assert_eq!(completions.class, "Widget");
    assert_eq!(completions.members.members.len(), 1);
}

#[test]
fn a_completion_on_this_offers_the_enclosing_class_members() {
    // `this` needs no inference — the scope chain already knows which class it is — which makes it the one
    // completion inside a class body that works before the object has a name.
    let source = "struct Widget {\n  int size;\n  void grow() {\n    this->\n  }\n};\n";
    let found = completions_at(&[], "/p/a.cpp", source, "this->");

    let Known::Yes(completions) = found else {
        panic!("`this` is the enclosing `Widget`: {found:?}");
    };
    assert_eq!(completions.class, "Widget");
    assert_eq!(
        completions
            .members
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>(),
        ["grow", "size"]
    );
}

#[test]
fn the_members_offered_include_the_ones_the_bases_declare() {
    // Completion is where the query-time base walk pays off: the whole inherited list is there, each entry
    // tagged with the class that declares it, without anything having been stored on `Derived`.
    let source = "struct Base {\n  int inherited_size;\n};\n\
                  struct Derived : public Base {\n  int own_size;\n};\n\
                  void f() {\n  Derived d;\n  d.\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "d.");

    let Known::Yes(completions) = found else {
        panic!("`d` is a `Derived`: {found:?}");
    };

    let listed: Vec<(&str, &str)> = completions
        .members
        .members
        .iter()
        .map(|member| (member.fact.name.as_str(), member.declared_in.as_str()))
        .collect();
    assert_eq!(
        listed,
        [("own_size", "Derived"), ("inherited_size", "Base")],
        "the class's own member first, then the inherited one, tagged with where it is declared"
    );
}

#[test]
fn the_members_offered_cross_a_header() {
    let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n  w.\n}\n";
    let (index, tree) = analysed_while_typing(
        &[("/p/widget.h", "struct Widget {\n  int size;\n};\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let found = super::member_completions_at(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/main.cpp"),
        after(source, "w."),
    );

    let Known::Yes(completions) = found else {
        panic!("the class is in the included header: {found:?}");
    };
    assert_eq!(completions.class, "Widget");
    assert_eq!(
        completions.members.members[0].file,
        Path::new("/p/widget.h")
    );
}

#[test]
fn a_completion_after_a_call_offers_what_the_call_returns() {
    // `make().` — the keystroke this capability exists for, and the one the test below is the boundary of.
    // Before `returns` existed this answered `UnknownType`; now it is the same answer as `w.` for a `w` of the
    // same type, which is the whole point of recording a return type at all.
    let source = "struct Widget {\n  int size;\n  void grow();\n};\nWidget make();\n\
                  void f() {\n  make().\n}\n";
    let Known::Yes(completions) = completions_at(&[], "/p/a.cpp", source, "make().") else {
        panic!("`make()` returns a `Widget`");
    };

    assert_eq!(completions.class, "Widget");
    assert_eq!(
        completions
            .members
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>(),
        ["grow", "size"]
    );
}

#[test]
fn a_completion_after_a_dereference_offers_the_pointees_members() {
    // The completion form of [`a_member_access_through_a_dereference_resolves`], and the one a user meets:
    // typing `(*make()).` should offer the `Widget`'s members, not nothing.
    //
    // This test asserted the *opposite* until the dereference was implemented — "a call's return type is not
    // computed yet" — and the failing assertion is what said the boundary had moved.
    let source = "struct Widget {\n  int size;\n};\nWidget* make();\n\
                  void f() {\n  (*make()).\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "(*make()).");

    let Known::Yes(completions) = found else {
        panic!("`(*make())` is a `Widget`, so its members are the answer: {found:?}");
    };
    assert_eq!(completions.class, "Widget");
}

#[test]
fn a_completion_on_an_expression_with_no_type_offers_nothing_and_says_why() {
    // What is left of the boundary, and this is where a user meets it. Offering the members of some other
    // class would be worse than offering none: the list looks like an answer.
    //
    // An **arithmetic expression** is the case that is still out of reach, and it is the honest one: `a + b`
    // has a type the language computes from conversions this layer does not model, so there is nothing to
    // read off a declaration.
    let source =
        "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget a;\n  (a.size + a.size).\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "(a.size + a.size).");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnknownType(_))),
        "an arithmetic expression's type is not read off a declaration: {found:?}"
    );
}

#[test]
fn a_completion_on_an_object_of_an_unknown_type_offers_nothing() {
    // The object resolves fine — `w`'s declaration *says* its type is `Nowhere` — and what fails is the type:
    // nothing here declares it. So the reason names the type rather than the object, which is what tells a
    // consumer whether to go looking for a missing declaration of `w` or of `Nowhere`.
    let source = "void f() {\n  Nowhere w;\n  w.\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "w.");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "`Nowhere` is a type nothing here declares: {found:?}"
    );
    let Known::Unknown(UnknownReason::NotDeclaredHere(name)) = found else {
        unreachable!("just checked")
    };
    assert_eq!(
        &*name, "Nowhere",
        "and the reason names the type, not the object"
    );
}

#[test]
fn a_cursor_that_is_not_on_a_member_access_offers_nothing() {
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "Widget w;");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
        "a declaration is not a member access: {found:?}"
    );
}

#[test]
fn a_cursor_on_the_object_asks_about_the_object() {
    // The offset decides, not the node: `w.size` with the cursor *on* `w` is a question about `w`, and
    // answering it with `Widget`'s members would insert a member name in the middle of the object's.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
    let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let found = super::member_completions_at(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/a.cpp"),
        at(source, "w.size"),
    );

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
        "the cursor is on the object, before the operator: {found:?}"
    );
}

#[test]
fn a_cursor_inside_a_name_filters_by_the_part_before_it() {
    // The range and the prefix answer two halves of one edit, and this is where they differ: the client
    // replaces all of `size`, while the filter matches `si` — the part the user has actually typed. Using the
    // whole spelling for both is what makes completion offer nothing in the middle of a word.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.size;\n}\n";
    let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    // Two characters into `size`.
    let cursor = at(source, "w.size") + 2 + 2;

    let found = super::member_completions_at(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/a.cpp"),
        cursor,
    );
    let Known::Yes(completions) = found else {
        panic!("the cursor is inside the member's name: {found:?}");
    };

    assert_eq!(completions.prefix, "si", "what has been typed so far");
    assert_eq!(
        &source[completions.member_range.start_offset..completions.member_range.end_offset()],
        "size",
        "and the whole name is what gets replaced"
    );
}

#[test]
fn a_completed_member_access_still_answers_with_its_own_prefix() {
    // The state between two keystrokes: `w.size` is a finished expression, and a cursor at its end is still
    // asking for the members — with `size` as the prefix to replace. Nothing about the completion path is
    // special to the half-typed case.
    let source = "struct Widget {\n  int size;\n  void grow();\n};\n\
                  void f() {\n  Widget w;\n  w.size\n}\n";
    let found = completions_at(&[], "/p/a.cpp", source, "w.size");

    let Known::Yes(completions) = found else {
        panic!("the cursor is at the end of `size`: {found:?}");
    };
    assert_eq!(completions.prefix, "size");
    assert_eq!(
        &source[completions.member_range.start_offset..completions.member_range.end_offset()],
        "size"
    );
}

#[test]
fn a_jump_on_a_member_that_is_not_written_yet_has_nothing_to_jump_to() {
    // The completion query tolerates a nameless access; the *definition* query must not, and the difference is
    // the question rather than the shape. Reading `w.` as "the member named nothing" would answer
    // `NotDeclaredHere("Widget::")`, which says the class is missing a member that has not been typed.
    let source = "struct Widget {\n  int size;\n};\nvoid f() {\n  Widget w;\n  w.\n}\n";
    let (index, tree) = analysed_while_typing(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let found = super::member_across_files(
        &index,
        &mut |_: &Path| None,
        &scopes,
        &root,
        Path::new("/p/a.cpp"),
        after(source, "w."),
    );

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
        "there is no written name to resolve: {found:?}"
    );
}

// -------------------------------------------------------------------------------------------
// Completion: the cursor-to-names query
//
// The sibling of the member query above, and the same kind of fixture: a *name being written* is not a
// program, so these files do not parse and are not meant to. What each test asserts is one of the four
// positions — after a `::`, after a class's `::`, after a global `::`, and with no qualifier at all — because
// they reach the same query through different scopes.
// -------------------------------------------------------------------------------------------

/// The names offered at the cursor just past `needle`.
fn names_at(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
    needle: &str,
) -> Known<super::NameCompletions> {
    let (index, tree) = analysed_while_typing(files, from, source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    super::name_completions_at(
        &index,
        &scopes,
        &root,
        Path::new(from),
        after(source, needle),
    )
}

/// The names offered at the **first offset of the body** of `source` — where nothing is written yet.
///
/// The position with no prefix, which is a different question from [`names_at`]'s and cannot be spelled as one:
/// a needle is what a test looks for, and "the cursor is at the start of a line" is not a spelling.
fn names_at_blank(
    files: &[(&str, &str)],
    from: &str,
    source: &str,
) -> Known<super::NameCompletions> {
    let (index, tree) = analysed_while_typing(files, from, source);
    let root = tree.get_red_root();
    let scopes = build_scopes(&root, &NoMacroBodies);

    let at = source
        .find("void f()")
        .and_then(|body| source[body..].find('{').map(|brace| body + brace + 1))
        .expect("the fixture has a body");

    super::name_completions_at(&index, &scopes, &root, Path::new(from), at)
}

/// The names in an answer, in the order the query put them.
#[track_caller]
fn offered(found: &Known<super::NameCompletions>) -> Vec<String> {
    match found {
        Known::Yes(completions) => completions
            .names
            .iter()
            .map(|name| name.fact.name.to_string())
            .collect(),
        other => panic!("expected names: {other:?}"),
    }
}

/// **A name is offered however far into its namespace's files it is written.**
///
/// The budget that used to apply here was spent in **file order**: the walk took whole files until it had
/// collected 400 declarations, so a namespace whose first file is large — every standard header is — pushed the
/// rest out of the answer. Measured on a file that includes `<cstdio>`, `<iostream>`, `<optional>`, `<string>`
/// and `<format>`: completion after `std::` offered **175** names, `cin` and `cout` among them, and `string`
/// (declared in `<xstring>`, one include further along) was not there at all — so the list a reader saw depended
/// on the order of their `#include` lines. See [`MAX_COLLECTED_IN_A_SCOPE`].
#[test]
fn a_name_in_a_late_file_of_a_namespace_is_still_offered() {
    // More than the old budget, so that the file after this one is never read under it.
    let mut big = String::from("namespace ns {\n");
    for index in 0..(super::MAX_COLLECTED_NAMES + 20) {
        big.push_str(&format!("  int filler{index};\n"));
    }
    big.push_str("}\n");

    let late = "namespace ns {\n  int zebra;\n}\n";
    // **`late.h` is reached through `big.h`**, which is the shape the defect had: the header that fills the
    // budget is a *direct* include of the file being typed in, and the one holding the wanted name is one step
    // further along the walk — `string` lives in `<xstring>`, which `<string>` includes.
    let big_with_include = format!("#include \"late.h\"\n{big}");
    let source = "#include \"big.h\"\nvoid f() {\n  ns::\n}\n";
    let files = [
        ("/p/big.h", big_with_include.as_str()),
        ("/p/late.h", late),
        ("/p/a.cpp", source),
    ];

    let found = names_at(&files, "/p/a.cpp", source, "ns::");
    let names = offered(&found);

    assert!(
        names.iter().any(|name| name == "zebra"),
        "the name is declared in a file the cursor can see, and the list must not stop before it: \
         {} names offered, none of them `zebra`",
        names.len()
    );
}

/// **The members of a class template reached through an alias**: `std::string::` is how a reader asks this
/// every day, and the class it names is not a scope *in the buffer's tree* — it is an alias declared in a header.
///
/// The query that answers it is the same one a member access uses ([`members_of`]), which resolves the alias, the
/// template spelling and the bases; asking only `declarations_in("std::string")` — what this used to do — finds
/// nothing at all, because no declaration in the program has that scope. Measured on a file that includes
/// `<string>`: **0** members before this, **63** of `basic_string`'s after.
#[test]
fn the_members_of_a_class_template_reached_through_an_alias_are_offered() {
    let header = "namespace ns {\n\
                  template <class _Ty>\n\
                  struct Basic {\n\
                    unsigned long size() const;\n\
                    _Ty* data();\n\
                  };\n\
                  using String = Basic<char>;\n\
                  }\n";
    let source = "#include \"basic.h\"\nvoid f() {\n  ns::String::\n}\n";
    let files = [("/p/basic.h", header), ("/p/a.cpp", source)];

    let found = names_at(&files, "/p/a.cpp", source, "ns::String::");
    let names = offered(&found);

    assert!(
        names.iter().any(|name| name == "size") && names.iter().any(|name| name == "data"),
        "`ns::String` is `ns::Basic<char>`, and its members are the answer: {names:?}"
    );
}

#[test]
fn a_qualified_scope_offers_the_names_written_in_it() {
    let source =
        "namespace ns {\n  struct Widget { };\n  int helper();\n}\nvoid f() {\n  ns::\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "ns::");

    assert_eq!(
        offered(&found),
        ["Widget", "helper"],
        "sorted within the scope"
    );
    let Known::Yes(completions) = found else {
        unreachable!("asserted above")
    };
    assert_eq!(completions.scope, "ns", "the scope as a lookup key");
    assert_eq!(completions.prefix, "", "nothing of the name is typed yet");
    assert_eq!(
        completions.name_range.start_offset,
        after(source, "ns::"),
        "the empty range sits just past the `::`, where the name goes"
    );
    assert_eq!(completions.name_range.length, 0);
}

#[test]
fn a_half_typed_name_keeps_the_prefix_and_replaces_the_whole_segment() {
    // The two halves of one edit, as in the member query: the range covers what the client replaces (`Wid`),
    // and the prefix is what it filters by — the part before the cursor.
    let source = "namespace ns {\n  struct Widget { };\n}\nvoid f() {\n  ns::Wid\n}\n";
    let Known::Yes(completions) = names_at(&[], "/p/a.cpp", source, "ns::Wid") else {
        panic!("the cursor is on the name being typed");
    };

    assert_eq!(completions.scope, "ns");
    assert_eq!(completions.prefix, "Wid");
    assert_eq!(
        completions.name_range.start_offset,
        at(source, "Wid"),
        "the range is the written segment, not the point"
    );
    assert_eq!(completions.name_range.length, 3);
}

#[test]
fn a_class_scope_offers_its_members_and_the_ones_its_bases_declare() {
    // `Derived::` is a scope like `ns::`, and the difference between them is the language's: a class is
    // defined once, and the names it has include the ones it inherits.
    let source = "struct Base {\n  typedef int size_type;\n};\n\
                  struct Derived : Base {\n  typedef int value_type;\n};\n\
                  void f() {\n  Derived::\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "Derived::");

    assert_eq!(
        offered(&found),
        ["value_type", "size_type"],
        "the class's own names first, then the ones a base declares"
    );
    let Known::Yes(completions) = found else {
        unreachable!("asserted above")
    };
    assert_eq!(completions.scope, "Derived");
    assert_eq!(
        completions
            .names
            .iter()
            .map(|name| name.depth)
            .collect::<Vec<_>>(),
        [0, 1],
        "the base's name is one step further out, which is the order lookup walks in"
    );
}

#[test]
fn a_global_scope_offers_only_what_is_declared_at_file_scope() {
    // `::` is a scope and not an empty qualifier: it asks about the global name space, so a namespace member
    // and a local are both *not* in the answer, while the file-scope declarations are — the enclosing function
    // and namespace included, because those are written there.
    let source =
        "int global;\nnamespace ns {\n  int inner;\n}\nvoid f() {\n  int local;\n  ::\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "::");

    assert_eq!(offered(&found), ["f", "global", "ns"]);
    let Known::Yes(completions) = found else {
        unreachable!("asserted above")
    };
    assert_eq!(completions.scope, "::", "the global name space, as spelled");

    let names = offered(&Known::Yes(completions.clone()));
    assert!(
        !names.contains(&"inner".to_string()) && !names.contains(&"local".to_string()),
        "a namespace member and a local are not in the global name space: {names:?}"
    );
}

#[test]
fn a_bare_name_offers_the_scopes_the_cursor_is_inside_innermost_first() {
    // No qualifier: every name visible from here, and the order is C++'s — the body's own, then the class's,
    // then the namespace's, then the file's.
    let source = "int at_file_scope;\nnamespace ns {\n  int in_a_namespace;\n  struct C {\n    int member;\n    \
                  void g() {\n      int local;\n      loc\n    }\n  };\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "loc");

    assert_eq!(
        offered(&found),
        [
            // depth 0: the body the cursor is in
            "local",
            // depth 1: the class — its members, `g` among them
            "g",
            "member",
            // depth 2: the namespace
            "C",
            "in_a_namespace",
            // depth 3: file scope, which is also where every included file writes
            "at_file_scope",
            "ns"
        ]
    );

    let Known::Yes(completions) = found else {
        unreachable!("asserted above")
    };
    assert_eq!(completions.scope, "", "no qualifier was written");
    assert_eq!(completions.prefix, "loc");

    // The order is not alphabetical and is not meant to be: it is the order C++ searches in, so a consumer
    // that shows the list in this order shows the name the user would get first.
    let depths: Vec<usize> = completions.names.iter().map(|name| name.depth).collect();
    let mut sorted = depths.clone();
    sorted.sort_unstable();
    assert_eq!(depths, sorted, "nearer scopes come first");
    assert_eq!(depths[0], 0, "the local is in the scope the cursor is in");
}

#[test]
fn another_files_local_is_not_offered_by_a_bare_name() {
    // The reason `local` is a field at all. The header's `__first` is a declaration in the index with no scope
    // to place it in, and the standard library's headers declare thousands of names like it — offering one
    // would fill the list with names the user cannot see, and the *right* answer for this cursor is the local
    // that really is here.
    let header = "/p/widget.h";
    let declaration = "void g() {\n  int __first = 0;\n  int helper;\n}\nint global_name;\n";
    let source = "#include \"widget.h\"\nvoid f() {\n  int mine;\n  min\n}\n";
    let found = names_at(&[(header, declaration)], "/p/main.cpp", source, "min");

    let names = offered(&found);
    assert!(
        names.contains(&"mine".to_string()),
        "the local of *this* file is offered, from its own scopes: {names:?}"
    );
    assert!(
        !names.contains(&"__first".to_string()) && !names.contains(&"helper".to_string()),
        "another file's locals are not names this file can write: {names:?}"
    );

    // **A name that does not begin with what is being typed is not collected**, which is a filter and not a
    // ranking: `global_name` cannot be completed from `min`, and a client would drop it. So the same question
    // is asked once more at the **blank** position the feature exists for, which is where the file-scope name
    // has to appear — asking it at `min` would be asserting two things at once.
    let at_a_blank = names_at_blank(&[(header, declaration)], "/p/main.cpp", source);
    let names = offered(&at_a_blank);
    assert!(
        names.contains(&"global_name".to_string()),
        "…while a file-scope name in the same header is offered where nothing is typed: {names:?}"
    );
}

#[test]
fn a_namespace_is_merged_across_the_buffer_and_the_header() {
    // A namespace can be reopened by every file that mentions it, so the buffer's names are never the whole
    // answer — the difference from a class, which is defined once and listed from one place.
    let source =
        "#include \"ns.h\"\nnamespace ns {\n  int in_the_buffer;\n}\nvoid f() {\n  ns::\n}\n";
    let found = names_at(
        &[("/p/ns.h", "namespace ns {\n  int in_the_header;\n}\n")],
        "/p/main.cpp",
        source,
        "ns::",
    );

    assert_eq!(offered(&found), ["in_the_buffer", "in_the_header"]);
}

#[test]
fn a_scope_nothing_declares_is_not_an_empty_list() {
    // "Declared and empty" and "no such scope" are different answers, and the second one is what stops a
    // consumer from reporting that the user is looking at a namespace with nothing in it.
    let source = "void f() {\n  nowhere::\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "nowhere::");

    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "nothing declares `nowhere`: {found:?}"
    );
}

#[test]
fn a_cursor_where_a_name_can_be_written_offers_the_visible_names() {
    // **The entry condition, which used to be the whole bug.** A cursor with no name written yet — at the start
    // of a declaration, after `return `, at the beginning of a line — is the position completion exists for.
    // This test used to assert the opposite ("the cursor is on a declaration's name, which is not a name being
    // written"), and a language server that answers nothing when a client asks at a blank position is one a
    // user reports as "it has no completion for local variables" — which is what happened.
    //
    // The replacement range is the *whole* identifier the cursor is on, so choosing a name replaces it rather
    // than inserting beside it.
    // The cursor is just past the name `Widget` — the position a client reports when the user puts the cursor
    // on the identifier and presses the completion key.
    let source = "struct Widget { int size; };\nvoid f() {\n  Widget w;\n}\n";
    let found = names_at(&[], "/p/a.cpp", source, "Widget");

    let Known::Yes(found) = found else {
        panic!("a name can be written here: {found:?}");
    };
    let names: Vec<&str> = found
        .names
        .iter()
        .map(|offered| offered.fact.name.as_str())
        .collect();
    assert!(
        names.contains(&"Widget") && names.contains(&"f"),
        "the names in scope: {names:?}"
    );
    assert_eq!(found.prefix, "Widget", "what is written filters the list");
    assert_eq!(
        found.name_range.length,
        "Widget".len(),
        "and the chosen name replaces it rather than being inserted beside it"
    );
}

// -------------------------------------------------------------------------------------------
// Macros
//
// Every test here is about **translation order**: which of the `#define`s and `#undef`s the
// preprocessor would have gone past last. The fixtures are written so that the *set* of facts is the
// same in several of them and only their order differs, because the set is what a naive
// implementation gets right and the order is what it gets wrong.
// -------------------------------------------------------------------------------------------

/// The line a found fact is on, so a test can say "the second one" without arithmetic.
fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count()
}

#[test]
fn a_macro_defined_above_its_use_resolves_to_the_define() {
    let source = "#define MAX 1\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

    let Known::Yes(found) = found else {
        panic!("the define above the use is the answer: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/a.cpp"));
    assert_eq!(found.fact.name, "MAX");
    assert!(found.fact.kind.is_definition());
    assert_eq!(
        &source[found.fact.range.start_offset..found.fact.range.end_offset()],
        "MAX",
        "and the range is the name a user would be sent to"
    );
}

#[test]
fn a_macro_defined_below_its_use_is_not_in_force_yet() {
    // The file is read top to bottom, so a `#define` further down the page is not what the name above it
    // means. A table keyed by name alone answers this one wrong, and it is the reason the query is positional.
    let source = "int x = MAX;\n#define MAX 1\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "nothing above the use defines it: {found:?}"
    );
}

#[test]
fn a_macro_in_an_included_header_resolves_into_that_header() {
    let source = "#include \"config.h\"\nint x = FEATURE;\n";
    let header = "#define FEATURE 1\n";
    let (index, tree) = analysed(&[("/p/config.h", header)], "/p/main.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );

    let Known::Yes(found) = found else {
        panic!("the header's define must be found: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/config.h"));
    assert_eq!(line_of(header, found.fact.range.start_offset), 0);
}

#[test]
fn a_header_included_after_the_use_does_not_define_it_yet() {
    let source = "int x = FEATURE;\n#include \"config.h\"\n";
    let (index, tree) = analysed(
        &[("/p/config.h", "#define FEATURE 1\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "the include is below the use: {found:?}"
    );
}

#[test]
fn the_later_of_two_headers_that_define_it_wins() {
    // The set of facts is identical in this test and the next one; only the order differs. That is the pair a
    // name-keyed implementation cannot tell apart.
    let source = "#include \"one.h\"\n#include \"two.h\"\nint x = FLAG;\n";
    let one = "#define FLAG 1\n";
    let two = "#define FLAG 2\n";
    let (index, tree) = analysed(
        &[("/p/one.h", one), ("/p/two.h", two)],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found =
        super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

    let Known::Yes(found) = found else {
        panic!("the last define gone past is the answer: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/two.h"));
}

#[test]
fn swapping_the_two_includes_swaps_the_answer() {
    let source = "#include \"two.h\"\n#include \"one.h\"\nint x = FLAG;\n";
    let one = "#define FLAG 1\n";
    let two = "#define FLAG 2\n";
    let (index, tree) = analysed(
        &[("/p/one.h", one), ("/p/two.h", two)],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found =
        super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "FLAG;"));

    let Known::Yes(found) = found else {
        panic!("order is the whole answer: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/one.h"));
}

#[test]
fn a_local_define_after_an_include_wins_over_the_header() {
    let source = "#include \"config.h\"\n#define FEATURE 2\nint x = FEATURE;\n";
    let (index, tree) = analysed(
        &[("/p/config.h", "#define FEATURE 1\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    let Known::Yes(found) = found else {
        panic!("the file's own later define is what is in force: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/main.cpp"));
}

#[test]
fn a_local_define_before_an_include_loses_to_the_header() {
    // The same two facts as the test above with the lines the other way round, which is the whole point: this
    // is a positional query, so it is not enough to know that both files define the name.
    let source = "#define FEATURE 2\n#include \"config.h\"\nint x = FEATURE;\n";
    let (index, tree) = analysed(
        &[("/p/config.h", "#define FEATURE 1\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    let Known::Yes(found) = found else {
        panic!("the header was pasted after the local define: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/config.h"));
}

#[test]
fn a_nested_include_is_reached() {
    let source = "#include \"middle.h\"\nint x = DEEP;\n";
    let (index, tree) = analysed(
        &[
            ("/p/middle.h", "#include \"leaf.h\"\n"),
            ("/p/leaf.h", "#define DEEP 1\n"),
        ],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found =
        super::macro_across_files(&index, &root, Path::new("/p/main.cpp"), at(source, "DEEP;"));

    let Known::Yes(found) = found else {
        panic!("a define two includes down is still in force: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/leaf.h"));
}

#[test]
fn an_undef_of_a_local_define_leaves_an_ordinary_identifier() {
    // The answer is not the `#define` it used to have: pointing there would send a user to a definition that is
    // not in force, which is a wrong answer rather than a missing one.
    let source = "#define MAX 1\n#undef MAX\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));

    assert!(
        matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
        "an `#undef` above the use settles it: {found:?}"
    );
}

#[test]
fn an_undef_of_a_headers_define_also_counts() {
    // Across a file boundary, which is the case a single-file macro table cannot see at all: the `#undef` is
    // in the querying file and the `#define` is in the header.
    let source = "#include \"config.h\"\n#undef FEATURE\nint x = FEATURE;\n";
    let (index, tree) = analysed(
        &[("/p/config.h", "#define FEATURE 1\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    assert!(
        matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
        "the header's define is ended by the file's own `#undef`: {found:?}"
    );
}

#[test]
fn a_define_after_an_undef_is_in_force_again() {
    let source = "#define MAX 1\n#undef MAX\n#define MAX 2\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
    let Known::Yes(found) = found else {
        panic!("the last fact is a define: {found:?}");
    };
    assert_eq!(line_of(source, found.fact.range.start_offset), 2);
}

#[test]
fn an_undef_before_a_conditional_define_is_not_absence() {
    // `<assert.h>` writes this shape: `#undef assert`, then one definition per configuration. The certain undef is
    // before every definition, so it must not settle the answer as "not a macro"; the later conditional definitions
    // are exactly the doubt `ConditionalCompilation` names.
    let source =
        "#undef MAX\n#if defined(ONE)\n#define MAX 1\n#else\n#define MAX 2\n#endif\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
    assert!(
        matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
        "the conditional definitions overturn the undef: {found:?}"
    );
}

#[test]
fn a_define_inside_a_conditional_is_unknown_rather_than_guessed() {
    let source = "#if defined(USE_MAX)\n#define MAX 1\n#endif\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
    assert!(
        matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
        "whether that branch was taken is not knowable here: {found:?}"
    );
}

#[test]
fn an_unconditional_define_beats_a_guarded_one_after_it() {
    // The rule the declaration query uses, applied to positions: the name certainly is a macro, so refusing to
    // answer would be refusing a question that has an answer. What the guarded define might do is stated in the
    // documentation rather than turned into an `Unknown`.
    let source = "#define MAX 1\n#if defined(OTHER)\n#define MAX 3\n#endif\nint x = MAX;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "MAX;"));
    let Known::Yes(found) = found else {
        panic!("the unconditional define is what is certain: {found:?}");
    };
    assert_eq!(line_of(source, found.fact.range.start_offset), 0);
}

#[test]
fn a_guarded_include_makes_the_answer_conditional() {
    let source = "#if defined(USE_CONFIG)\n#include \"config.h\"\n#endif\nint x = FEATURE;\n";
    let (index, tree) = analysed(
        &[("/p/config.h", "#define FEATURE 1\n")],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    assert!(
        matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
        "the only path to the define goes through an `#if`: {found:?}"
    );
}

#[test]
fn a_name_no_one_defines_is_not_declared_here() {
    let source = "int x = NOT_A_MACRO;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/a.cpp"),
        at(source, "NOT_A_MACRO;"),
    );
    assert!(
        matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
        "the index is a subset of the translation unit, so this is not a definite no: {found:?}"
    );
}

#[test]
fn a_cursor_that_is_not_on_a_name_has_nothing_to_look_up() {
    let source = "#define MAX 1\nint x = MAX + 1;\n";
    let (index, tree) = analysed(&[], "/p/a.cpp", source);
    let root = tree.get_red_root();

    let found = super::macro_across_files(&index, &root, Path::new("/p/a.cpp"), at(source, "+ 1;"));
    assert!(
        matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
        "the cursor is on an operator: {found:?}"
    );
}

#[test]
fn a_cycle_of_includes_terminates() {
    // Two headers that include each other, both defining the name: the walk expands a file once per query, so
    // it ends instead of growing a chain for ever.
    let source = "#include \"a.h\"\nint x = SHARED;\n";
    let (index, tree) = analysed(
        &[
            ("/p/a.h", "#include \"b.h\"\n#define SHARED 1\n"),
            ("/p/b.h", "#include \"a.h\"\n"),
        ],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "SHARED;"),
    );
    let Known::Yes(found) = found else {
        panic!("a cycle must not stop the define being found: {found:?}");
    };
    assert_eq!(found.file, Path::new("/p/a.h"));
}

#[test]
fn an_undef_in_a_header_is_reached_like_a_define() {
    // The symmetric case, and the reason `#undef` is a fact rather than a note: the fact that settles the
    // question can be in either file.
    let source = "#include \"config.h\"\n#include \"cleanup.h\"\nint x = FEATURE;\n";
    let (index, tree) = analysed(
        &[
            ("/p/config.h", "#define FEATURE 1\n"),
            ("/p/cleanup.h", "#undef FEATURE\n"),
        ],
        "/p/main.cpp",
        source,
    );
    let root = tree.get_red_root();

    let found = super::macro_across_files(
        &index,
        &root,
        Path::new("/p/main.cpp"),
        at(source, "FEATURE;"),
    );
    assert!(
        matches!(found, Known::Unknown(UnknownReason::UndefinedHere(_))),
        "the later header undefines it: {found:?}"
    );
}

/// **A reading that rendered to nothing does not displace one with declarations.**
///
/// This is `nlohmann/json.hpp` at the layer that answered wrongly: the on-its-own reading held **761** declarations,
/// a program reading filed after it held **0**, and every lookup made from that program saw the empty one — measured
/// as `declarations_in("nlohmann::basic_json") == 53` (the raw summary's members) while the class's members were in
/// the index. The insert refuses the empty reading now; this pins the refusal at the query a reader feels.
#[test]
fn an_empty_program_reading_does_not_hide_the_one_with_declarations() {
    let mut index = index(&[
        (
            "/p/a.h",
            "struct Widget {\n    int size;\n    int area();\n};\n",
        ),
        (
            "/p/main.cpp",
            "#include \"a.h\"\nint main() { return 0; }\n",
        ),
    ]);
    let cooked_of = |index: &ProjectIndex, path: &str| {
        let summary = index
            .summary(Path::new(path))
            .expect("the fixture is summarized");
        index::CookedFile::declarations(summary.declarations.clone())
    };
    // The reading a render produces for the header, filed on its own.
    index.insert_cooked(Path::new("/p/a.h"), cooked_of(&index, "/p/a.h"));
    // A program that read `main.cpp`, and — the defect — an empty reading it produced for the header.
    index.insert_cooked_in_a_program(
        Path::new("/p/main.cpp"),
        7,
        cooked_of(&index, "/p/main.cpp"),
    );
    index.insert_cooked_in_a_program(Path::new("/p/a.h"), 7, index::CookedFile::default());

    let members = index
        .declarations_in("Widget", Path::new("/p/main.cpp"))
        .len();
    assert!(
        members >= 2,
        "the class's members are still visible from the program that read the header: {members}"
    );
    let found = index.definition("Widget::size", Path::new("/p/main.cpp"));
    assert!(
        matches!(found, Known::Yes(_)),
        "and a member resolves: {found:?}"
    );
}

/// **A body edit reuses the completion snapshot.** The header's global names are the expensive half; a
/// function body changing at new offsets is not a declaration change and must not rebuild them.
#[test]
fn a_body_edit_keeps_the_completion_snapshot() {
    let mut index = ProjectIndex::new();
    let header = summarize(
        Path::new("/p/widget.h"),
        "struct Widget { int size; };\n",
        SummaryKey::new(0, 0),
    );
    let main = "/p/main.cpp";
    let before = "#include \"widget.h\"\nvoid f() { int a = 1; }\n";
    let after = "#include \"widget.h\"\nvoid f() { int b = 2; }\n";
    let mut summary = summarize(Path::new(main), before, SummaryKey::new(0, 0));
    for include in &mut summary.includes {
        include.resolved = Some(std::path::PathBuf::from("/p/widget.h"));
    }
    index.insert(header);
    index.insert(summary);

    let definition = index.definition("Widget", Path::new(main));
    assert!(matches!(definition, Known::Yes(_)));
    let cached_before = index
        .definition_answers
        .lock()
        .expect("the definition memo")
        .len();
    assert!(cached_before > 0);

    let first = index.completion_visible_declarations(Path::new(main));
    index.forget(Path::new(main));
    let mut edited = summarize(Path::new(main), after, SummaryKey::new(0, 1));
    for include in &mut edited.includes {
        include.resolved = Some(std::path::PathBuf::from("/p/widget.h"));
    }
    index.insert(edited);
    let second = index.completion_visible_declarations(Path::new(main));
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "a body edit rebuilt the completion snapshot"
    );
    assert_eq!(
        cached_before,
        index
            .definition_answers
            .lock()
            .expect("the definition memo")
            .len(),
        "a body edit threw away a header's definition answer"
    );

    let mut header = summarize(
        Path::new("/p/widget.h"),
        "struct Widget { int size; };\nstruct Extra { int value; };\n",
        SummaryKey::new(1, 0),
    );
    for include in &mut header.includes {
        include.resolved = None;
    }
    index.forget(Path::new("/p/widget.h"));
    index.insert(header);
    let third = index.completion_visible_declarations(Path::new(main));
    assert!(
        !std::sync::Arc::ptr_eq(&first, &third),
        "a declaration change did not rebuild the completion snapshot"
    );
}
