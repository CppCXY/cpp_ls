//! **A qualified name that goes through an alias to a template instantiation** — `ns::simple::make()` where
//! `using simple = basic<int>;`.
//!
//! Reported from an editor on `CodeFormatServer/src/LSP/LSP.cpp`: `nlohmann::json::object()` had **no hover card**.
//! `nlohmann` spells the type the same way this fixture does — `using json = basic_json<>;` — so the qualified name is
//! namespace → alias → template instantiation → static member, and the lookup that fails is the whole chain asked as
//! one spelling.

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

const LIBRARY: &str = "\
namespace ns {

template <class T = int>
struct basic {
    static basic make();
    T value;
};

using simple = basic<int>;
using defaulted = basic<>;

}
";

/// **`ns::simple::make` is `basic<int>`'s `make`.** The qualifier names an alias, not a class; the alias's target is
/// what has the member, and the declaration a reader wants is that member's.
#[test]
fn a_member_through_an_alias_is_the_aliased_templates_member() {
    let written = "\
#include \"lib.h\"

int main() {
    auto made = ns::simple::make();
    return 0;
}
";
    let session = session_of(&[("/p/lib.h", LIBRARY), ("/p/main.cpp", written)]);
    let found = where_at(&session, "/p/main.cpp", "make");
    assert!(
        found.ends_with("::make") && found.contains("basic"),
        "the alias resolves to basic<int>::make, not nothing: {found}"
    );
}

/// **The same through a target with default arguments** — `using defaulted = basic<>;`, which is the spelling
/// `nlohmann` uses for `json`.
#[test]
fn a_member_through_an_alias_with_default_arguments_is_the_same_member() {
    let written = "\
#include \"lib.h\"

int main() {
    auto made = ns::defaulted::make();
    return 0;
}
";
    let session = session_of(&[("/p/lib.h", LIBRARY), ("/p/main.cpp", written)]);
    let found = where_at(&session, "/p/main.cpp", "make");
    assert!(
        found.ends_with("::make") && found.contains("basic"),
        "basic<> is basic<int>, and make is its member: {found}"
    );
}

const BASIC: &str = "\
namespace ns {

template <class T = int>
struct basic {
    static basic make();
    T value;
};

}
";

const ALIAS: &str = "\
#pragma once
#include \"basic.h\"

namespace ns {

using simple = basic<>;

}
";

/// **The alias and the class in different files** — `nlohmann`'s own arrangement (`json` is aliased in
/// `json_fwd.hpp`, `basic_json` is defined in `json.hpp`). One file's summary records the alias, the other's the
/// member, and the qualified name has to cross that seam.
#[test]
fn an_alias_in_one_file_resolves_a_member_in_another() {
    let written = "\
#include \"alias.h\"

int main() {
    auto made = ns::simple::make();
    return 0;
}
";
    let session = session_of(&[
        ("/p/basic.h", BASIC),
        ("/p/alias.h", ALIAS),
        ("/p/main.cpp", written),
    ]);
    let found = where_at(&session, "/p/main.cpp", "make");
    println!("found: {found}");
    assert!(
        found.ends_with("::make") && found.contains("basic"),
        "the alias in another file resolves to basic<>::make: {found}"
    );
}
const PROBE_SHAPE: &str = "\
#include \"alias.h\"

struct Holder {
    void probe();
};

void Holder::probe() {
    auto made = ns::simple::make();
}
";

/// **The same qualified name inside a definition written with a qualifier** — `void Holder::probe() { … }`. The rule
/// that answers an *unqualified* name from the definition's own class must not touch a qualified one: measured on
/// `nlohmann/json.hpp`, `nlohmann::json::object` inside `WorkspaceDiagnosticReport::Serialize` was read as a member of
/// that class and answered `NotDeclaredHere`.
#[test]
fn a_qualified_name_inside_a_member_definition_is_not_the_classes_member() {
    let session = session_of(&[
        ("/p/basic.h", BASIC),
        ("/p/alias.h", ALIAS),
        ("/p/probe.cpp", PROBE_SHAPE),
    ]);
    let found = where_at(&session, "/p/probe.cpp", "make");
    println!("found: {found}");
    assert!(
        found.ends_with("::make") && found.contains("basic"),
        "the qualified name is resolved through the alias, not as Holder's member: {found}"
    );
}

/// **A probe: what the index records for an alias**, which is what an ask needs to find the class's file.
#[test]
#[ignore = "a probe: prints the alias fact the index holds"]
fn the_alias_fact_fields() {
    let session = session_of(&[
        ("/p/basic.h", BASIC),
        ("/p/alias.h", ALIAS),
        (
            "/p/main.cpp",
            "#include \"alias.h\"\nint main() { return 0; }\n",
        ),
    ]);
    for name in ["ns::simple", "ns::basic", "ns::simple::make"] {
        match session
            .index()
            .definition(name, std::path::Path::new("/p/main.cpp"))
        {
            Known::Yes(found) => println!(
                "[fact] {name}: kind {:?}, scope {:?}, type_of {:?}, returns {:?}",
                found.fact.kind, found.fact.scope, found.fact.type_of, found.fact.returns
            ),
            other => println!("[fact] {name}: {:?}", other),
        }
    }
}
