//! **What `ProjectIndex::definition` answers for a qualified spelling** — the probe behind the one gap left open in
//! `tests/alias_qualified_members.rs`.
//!
//! ```text
//! cargo test -p cpp_code_analysis --test index_qualified_probe -- --ignored --nocapture
//! ```
//!
//! Three files: a class template in one header, an alias to an instantiation in another, and a `.cpp` that includes the
//! alias header and uses `ns::simple::make()`. The session resolves the member; the index answered `NotDeclaredHere`
//! for every qualified spelling in the same fixture, which is the disagreement this prints.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

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

const PROBE: &str = "\
#include \"alias.h\"

struct Holder {
    void probe();
};

void Holder::probe() {
    auto made = ns::simple::make();
}
";

fn short(answer: &Known<cpp_code_analysis::ProjectDefinition>) -> String {
    match answer {
        Known::Yes(found) => format!(
            "Yes({}::{} in {:?})",
            found.fact.scope.as_deref().unwrap_or("(none)"),
            found.fact.name,
            found.file.file_name().unwrap_or_default()
        ),
        Known::Unknown(reason) => format!("Unknown({reason:?})"),
        Known::No => "No".to_string(),
    }
}

#[test]
#[ignore = "a probe: prints what the index answers for qualified spellings"]
fn qualified_spellings_in_the_three_file_fixture() {
    let files = [
        ("/p/basic.h", BASIC),
        ("/p/alias.h", ALIAS),
        ("/p/probe.cpp", PROBE),
    ];
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(path, text);
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
    for path in &paths {
        session.load(path);
    }
    session.index_everything();

    for path in ["/p/basic.h", "/p/alias.h", "/p/probe.cpp"] {
        let summary = session.index().summary(Path::new(path));
        println!(
            "[summary] {path}: {}",
            match summary {
                Some(summary) => format!("{} declaration(s)", summary.declarations.len()),
                None => "none".to_string(),
            }
        );
    }
    for name in ["ns", "ns::basic", "ns::simple", "basic", "simple"] {
        let answer = session.index().definition(name, Path::new("/p/probe.cpp"));
        println!("[index] from probe.cpp {name}: {}", short(&answer));
    }
    for name in ["ns::basic::make", "ns::simple::make", "make"] {
        let answer = session.index().definition(name, Path::new("/p/basic.h"));
        println!("[index] from basic.h {name}: {}", short(&answer));
    }
    let view = session
        .view_of_the_file(Path::new("/p/probe.cpp"))
        .expect("the probe has a reading");
    let at = view.source.find("make").expect("the call is written");
    let answer = session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
    );
    println!("[session] probe.cpp make: {}", short(&answer));
}
