//! **A probe, not a test**: why `std::vector` stops resolving in the modules project's `main.cpp`.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test module_hover_probe -- --ignored --nocapture
//! ```
//!
//! The project is `C:\Users\zc\Desktop\cpp_modules` — the fixture whose `main.cpp` imports `std`, `math` and `report`
//! and includes one header. The reader's report is that `std::vector` has no hover and no completion in that file, after
//! it worked before, and the question is which layer lost it: the unit read, the visibility walk, or the query.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_MODULES_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\cpp_modules".to_string()),
    )
}

#[test]
#[ignore]
fn why_std_vector_stops_resolving() {
    let root = project();
    if !root.join("src").join("main.cpp").exists() {
        println!("skipped: {} is not here", root.display());
        return;
    }
    let started = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();
    println!("settled in {:.1} s", started.elapsed().as_secs_f64());

    let main = root.join("src").join("main.cpp");
    let text = std::fs::read_to_string(&main).expect("main.cpp reads");
    session.did_open(&main, &text);
    session.index_everything();

    // **The query hover makes**: a name, a file, and "where is it defined".
    println!(
        "definition(\"std::vector\"): {:?}",
        session.index().definition("std::vector", &main).map_debug()
    );
    // And the bare spelling, which is what a hover on the `vector` token asks after the qualifier is stripped.
    println!(
        "definition(\"vector\"): {:?}",
        session.index().definition("vector", &main).map_debug()
    );
    println!(
        "definition(\"std::format\"): {:?}",
        session.index().definition("std::format", &main).map_debug()
    );

    // **What the index holds under that name**, so the answer above can be read against its candidates.
    let mut facts = session.index().every_fact_named("vector").into_iter();
    for _ in 0..10 {
        let Some((file, fact)) = facts.next() else {
            break;
        };
        println!(
            "  fact: scope {:?} kind {:?} definition {} exported {} file {}",
            fact.scope,
            fact.kind,
            fact.definition,
            fact.exported,
            file.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    // **The member list**, which is what a completion after `widgets.` and a hover's detail line are built from.
    let view = session.view_of_the_file(&main).expect("a view of main.cpp");
    match session.members_of(&view, "std::vector") {
        Known::Yes(members) => println!("members_of(\"std::vector\"): {} member(s)", members.members.len()),
        other => println!("members_of(\"std::vector\"): {other:?}"),
    }

    // **How much of `std` the file can see at all** — a namespace visible with few declarations is a unit-read problem;
    // a namespace with thousands and no `vector` is a query problem.
    let in_std = session.index().declarations_in("std", &main);
    println!("declarations_in(\"std\", main.cpp): {}", in_std.len());
    // **Which files those come from** — a namespace visible through one header is a different failure from a namespace
    // whose unit read never registered its closure.
    let mut where_from: Vec<String> = in_std
        .iter()
        .map(|declaration| declaration.file.to_string_lossy().to_string())
        .collect();
    where_from.sort();
    where_from.dedup();
    for file in where_from.iter().take(6) {
        println!("  std declared in: {file}");
    }
    println!("  distinct files: {}", where_from.len());
    println!(
        "unit_closure_of(\"std\"): {:?}",
        session.index().unit_closure_of("std").map(|files| files.len())
    );
    // **Does asking for the file's own units fix it?** The reader's hover asks a query, not a read; if the read is
    // what is missing, this one call tells the two apart.
    let before = Instant::now();
    let read = session.read_the_modules_a_file_imports(&main);
    println!("read_the_modules_a_file_imports: {read} unit(s) in {:.0} ms", before.elapsed().as_secs_f64() * 1000.0);
    println!(
        "  after the read: definition(\"std::vector\"): {:?}",
        session.index().definition("std::vector", &main).map_debug()
    );
    println!(
        "  after the read: unit_closure_of(\"std\") = {:?}",
        session.index().unit_closure_of("std").map(|files| files.len())
    );
    println!(
        "  after the read: declarations_in(\"std\") = {}",
        session.index().declarations_in("std", &main).len()
    );

    let config = session.config_for(&main);
    println!(
        "config: standard {:?}, {} include path(s), {} define(s)",
        config.standard,
        config.include_paths.len(),
        config.defines.len()
    );
}

/// `Known`'s own `Debug` is enough; this exists so the print above reads as one line.
trait MapDebug {
    fn map_debug(self) -> String;
}

impl MapDebug for Known<cpp_code_analysis::ProjectDefinition> {
    fn map_debug(self) -> String {
        match self {
            Known::Yes(found) => format!(
                "Yes({}, {:?}, definition {})",
                found.file.display(),
                found.fact.scope,
                found.fact.definition
            ),
            Known::Unknown(reason) => format!("Unknown({reason:?})"),
            Known::No => "No".to_string(),
        }
    }
}
