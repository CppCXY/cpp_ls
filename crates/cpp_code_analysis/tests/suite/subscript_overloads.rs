//! **A subscript on a class reached through an alias** — `ns::Json table; table["k"]`, where `Json` is `Table` in
//! another file.
//!
//! The overload ranking needs the class's **whole overload set** (`member_candidates`), and that query is a *scope*
//! posting keyed by the class's **qualified** name. Following an alias handed it the target's own spelling, which is
//! written unqualified inside the alias's namespace — `namespace ns { using Json = Table; }` — so the query asked for
//! `Table` and found nothing while `members_of("Table")` listed every member. With no candidates the ranking never
//! ran and the *first* declaration was taken:
//!
//! ```text
//! ns::Json table; table[0]     int&     ✓  the first declaration happens to be the one `0` selects
//! ns::Json table; table["k"]   int&     ✗  a compiler answers `double&`
//! ```
//!
//! Both classes below are declared in a header, so the query takes the index branch — the one the qualification
//! belongs to. A class in the file being edited is read from its own scope tree, where the name is already the one
//! the bindings were filed under, which is why the defect needed two files to be visible.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const HEADER: &str = "\
#pragma once
namespace ns {

struct Table {
    int& operator[](int at);
    double& operator[](const char* key);
};

struct Boxed {
    using reference = int&;
    using const_reference = const int&;
    reference operator[](int at);
    const_reference operator[](int at) const;
};

using Json = Table;
using Crate = Boxed;

}
";

const SOURCE: &str = "\
#include \"t.h\"

int by_index(ns::Json table) { return table[0]; }
double by_key(ns::Json table) { return table[\"k\"]; }
int by_const(const ns::Crate crate) { return crate[0]; }
int by_mutable(ns::Crate boxed) { return boxed[0]; }
";

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, source) in files {
        memory = memory.with_file(*path, *source);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| std::path::PathBuf::from(*path)),
    );
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session.load(std::path::Path::new("/p/a.cpp"));
    session
}

/// The type of the subscript whose spelling is `expression` — the offset is the byte of its `[`, so the question lands
/// on the index expression rather than on the base or the index.
fn the_subscripts_type(session: &Session<MemoryFiles>, expression: &str) -> String {
    let view = session.view("/p/a.cpp").expect("the file has a rendering");
    let at = SOURCE
        .find(expression)
        .unwrap_or_else(|| panic!("{expression} is written"));
    let bracket = at + expression.find('[').expect("a bracket");
    let reading = view
        .reading_offset_of(FileOffset::new(bracket))
        .expect("the cursor is in the rendering");
    match session.type_at(&view, reading) {
        Known::Yes(found) => found.type_of,
        other => format!("{other:?}"),
    }
}

#[test]
fn the_arguments_choose_the_overload_through_an_alias() {
    let session = session_with(&[("/p/t.h", HEADER), ("/p/a.cpp", SOURCE)]);

    assert_eq!(
        the_subscripts_type(&session, "table[0]"),
        "int&",
        "an `int` index selects `int& operator[](int)`"
    );
    assert_eq!(
        the_subscripts_type(&session, "table[\"k\"]"),
        "double&",
        "a `const char*` key selects `double& operator[](const char*)`, the second declaration"
    );
}

#[test]
fn the_objects_constness_chooses_the_overload_through_an_alias() {
    let session = session_with(&[("/p/t.h", HEADER), ("/p/a.cpp", SOURCE)]);

    assert_eq!(
        the_subscripts_type(&session, "crate[0]"),
        "const int&",
        "a `const` object can only call the `const` overload"
    );
    assert_eq!(
        the_subscripts_type(&session, "boxed[0]"),
        "int&",
        "while a mutable object keeps the non-const one"
    );
}
