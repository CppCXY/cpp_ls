//! **The reading a compiler gets** — a class whose member exists only after the file is rendered, and the query that
//! makes the session render it.
//!
//! `nlohmann/json.hpp` is the real case: `#if JSON_DIAGNOSTICS` is **0** in a header it includes, so the `JSON_TRY { … }`
//! block inside `basic_json::assert_invariant` is dead code a compiler never reads — and the raw parse still does,
//! which is what swallowed the rest of the class and left `basic_json::object` undeclared. The fixture here is that
//! shape in miniature, with the ask and the pump that are supposed to fix it.

use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const BASE: &str = "\
#pragma once
#define DIAGNOSTICS 0
#define TRY_STATEMENT try
#define CATCH_STATEMENT(exception) catch (exception)
";

const CLASS: &str = "\
#pragma once
#include \"base.h\"

struct Widget {
    int value = 0;

    void invariant() const noexcept
    {
#if DIAGNOSTICS
        TRY_STATEMENT
        {
            value = 1;
        }
        CATCH_STATEMENT(...) {}
#endif
        static_cast<void>(value);
    }

    static void after_the_method() {}

    int also_after = 0;
};
";

/// **The header is two levels down**, which is where the real report sat: `LSP.cpp` includes `LSP.h`, and `LSP.h`
/// includes `nlohmann/json.hpp`. The session's closure marking covers the files a request names and their direct
/// includes; the second level is what nothing queued.
const MIDDLE: &str = "\
#pragma once
#include \"class.h\"
";

const MAIN: &str = "\
#include \"middle.h\"

void probe() {
    Widget::after_the_method();
}
";

fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
        paths.push(PathBuf::from(path));
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(paths.clone());
    // **Only the .cpp is open.** A header the editor never opened is the case this fixture is about: it has a
    // summary and no rendering, and the query is what asks for one.
    for (path, text) in files {
        let path = PathBuf::from(path);
        session.load(&path);
        if path == Path::new("/p/main.cpp") {
            session.did_open(&path, text);
        }
    }
    session.index_everything();
    session
}

fn where_at(session: &Session<MemoryFiles>, path: &str, needle: &str) -> String {
    let path = Path::new(path);
    let Some(view) = session.view_of_the_file(path) else {
        return "no view".to_string();
    };
    let Some(at) = view.source.find(needle) else {
        return "no needle".to_string();
    };
    match session.definition(&view, ReadingOffset::new(at)) {
        Known::Yes(found) => format!(
            "{}::{}",
            found.fact.scope.as_deref().unwrap_or("(none)"),
            found.fact.name
        ),
        Known::Unknown(reason) => format!("unknown: {reason:?}"),
        Known::No => "no".to_string(),
    }
}

/// **The member is absent from the raw reading, the query asks for the rendered one, and the pump builds it.**
///
/// The order is the contract: a query that cannot answer says what file it needs read (`Session::view`'s ask), the
/// pump renders that file, and the next query answers. Before this change the pump never served the ask — measured,
/// `pending_cooking()` stayed at 33 across 4 000 `advance` calls — so the second query was as blind as the first.
#[test]
fn a_member_the_raw_reading_lost_is_answered_after_the_pump_renders_the_file() {
    let mut session = session_of(&[
        ("/p/base.h", BASE),
        ("/p/class.h", CLASS),
        ("/p/middle.h", MIDDLE),
        ("/p/main.cpp", MAIN),
    ]);

    // **The premise**: `class.h` is read as its own text, so the class body is the one a raw parse produces, and the
    // member written after the dead block is lost. If this ever passes, the fixture stopped reproducing anything.
    let first = where_at(&session, "/p/main.cpp", "after_the_method");
    let summary = session.index().summary(Path::new("/p/class.h"));
    let declared = summary.as_ref().map(|summary| {
        summary
            .declarations
            .iter()
            .filter(|fact| fact.name.as_str() == "after_the_method")
            .map(|fact| fact.scope.as_deref().unwrap_or("(none)"))
            .collect::<Vec<_>>()
    });
    println!(
        "before the pump: {first} | class.h summary: {:?} declarations, after_the_method in {:?}, cooked {:?}",
        summary.as_ref().map(|summary| summary.declarations.len()),
        declared,
        session
            .index()
            .cooked_declarations(Path::new("/p/class.h"))
            .map(|cooked| cooked.len())
    );

    // **The ask and the answer**: the query above asked for `class.h`'s rendering; the pump builds it.
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 500 {
        session.advance(64);
        slices += 1;
    }
    let cooked = session
        .index()
        .cooked_declarations(Path::new("/p/class.h"))
        .map(|declarations| declarations.len());
    println!(
        "pump: {slices} slice(s), pending {}, class.h cooked: {cooked:?}",
        session.pending_work()
    );

    let second = where_at(&session, "/p/main.cpp", "after_the_method");
    println!("after the pump: {second}");
    assert_eq!(
        second, "Widget::after_the_method",
        "the rendered reading of the class's own header holds the member"
    );
}
