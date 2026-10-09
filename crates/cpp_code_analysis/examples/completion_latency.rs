//! **What a completion costs while the index is filling** — the in-process twenty lines behind
//! `crates/cpp_ls/tests/latency.rs`, so that a fix can be measured without a subprocess, a JSON-RPC harness and a
//! 45-second wait.
//!
//! ```text
//! cargo run --release --example completion_latency -- <dir> [<file.cpp>]
//! ```
//!
//! The server's own log line already separates the three costs in front of the query (`completion: prepare … ms,
//! catch up … ms, modules … ms`) from the query itself (`completion: view … ms, completions … ms`), and on the
//! latency fixture that breakdown answers half the question: **the query is the whole of it.** Measured over the
//! wire: 0–77 ms in front of the query, and **2067 / 6285 / 17852 / 10790 ms** inside it, against 35–41 ms once the
//! index settles.
//!
//! What the server cannot say is which *part* of the query, because the query is one call. So this probe measures
//! the same distance in two steps:
//!
//! ```text
//! 1.  the pump's own alternation        `advance(INDEX_SLICE)`, then one completion at one cursor
//!                                       → the numbers `tests/latency.rs` asserts on, without the wire
//! 2.  the query, against a settled index and then a filling one
//!                                       → whether a part's cost rises with the number of inserts, which is what
//!                                         tells a cache being invalidated from work that is simply large
//! ```
//!
//! # The fixture
//!
//! The directory has to be one the analysis can configure itself, and `cl.exe` is not on `PATH` on this machine, so
//! it needs a `compile_commands.json` naming the include directories. `main.cpp` is written by the probe from
//! [`MAIN_CPP`], so the buffer and the disk agree: a `did_open` of text the disk does not have would have the probe
//! measuring a dropped summary being rebuilt, which is a different question.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The file the cursor queries are asked in, which is the latency fixture's own.
const MAIN_CPP: &str = r#"#include <vector>
#include <string>
#include <format>
#include <iostream>

class Sux {
public:
    Sux() : data(0) {}
    Sux(int value) : data(value) {}

    void print(int i) {
        printf("Sux class\n %d\n", this->data);
    }
private:
    int data;
};

int main() {
    Sux sux;
    std::string ixx;
    sux.print(ixx.size());
    std::cout << std::format("{}", 0) << std::endl;
    std::vector<int> v;
    v.push_back(42);
    std::string s = std::format("{}", 1);
    auto x = std::vector<int>();
    return 0;
}
"#;

/// How many files one slice reads — the server's own `INDEX_SLICE`, so the alternation is the pump's.
const INDEX_SLICE: usize = 16;

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("a directory with compile_commands.json"),
    );
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );

    match session.toolchain().and_then(|toolchain| toolchain.version.clone()) {
        Some(version) => println!("toolchain: {version}"),
        None => println!("toolchain: none discovered (the compile database is the configuration)"),
    }
    println!(
        "{} include paths | {} project files",
        session.config().include_paths.len(),
        session.project_files().len()
    );

    std::fs::write(&file, MAIN_CPP).expect("the fixture writes");
    let (line, column) = position_of(MAIN_CPP, "std::cout");
    let cursor = offset_in(MAIN_CPP, "std::cout").expect("the cursor is in the fixture");
    println!("cursor at line {line}, column {column} (offset {cursor})\n");

    session.did_open(&file, MAIN_CPP);

    // ---------------------------------------------------------------------------------------------
    // 1. The pump's alternation
    // ---------------------------------------------------------------------------------------------
    println!("--- advance({INDEX_SLICE}) then one completion ---");
    let mut worst = Duration::ZERO;
    let mut attempt = 0usize;
    while attempt < 24 {
        session.advance(INDEX_SLICE);

        let took = time_completion(&session, &file, cursor);
        worst = worst.max(took);
        println!(
            "  #{attempt:<2} {:<9} ms | pending {:<4} cooking {:<4} indexed {}",
            took.as_millis(),
            session.pending(),
            session.pending_cooking(),
            session.index().len()
        );

        attempt += 1;
        let settled = session.pending() == 0 && session.pending_cooking() == 0;
        if settled && attempt > 4 {
            break;
        }
    }
    println!("  worst: {} ms\n", worst.as_millis());

    // ---------------------------------------------------------------------------------------------
    // 2. The query's own parts
    // ---------------------------------------------------------------------------------------------
    println!("--- the same query, its parts told apart ---");
    let view = time_view(&session, &file);
    let whole = time_completion(&session, &file, cursor);
    let bare = session.view_of_the_file(&file).map(|view| {
        let started = Instant::now();
        let _ = session.completions(&view, 0);
        started.elapsed()
    });
    println!("  the file's view (a parse)        {} ms", view.as_millis());
    println!("  view + query (what a handler pays) {} ms", whole.as_millis());
    println!(
        "  the query, offset 0, view outside  {} ms",
        bare.unwrap_or_default().as_millis()
    );

    // ---------------------------------------------------------------------------------------------
    // 3. Which part of the query, on a settled index and on a filling one
    // ---------------------------------------------------------------------------------------------
    //
    // The `CPPLS_TRACE_COMPLETION=1` window is per query rather than cumulative: it is reset immediately before
    // each one, so the line below is about that query and not about the run.
    println!("\n--- the query's regions, per query (needs CPPLS_TRACE_COMPLETION=1) ---");
    let settled = session.pending() == 0 && session.pending_cooking() == 0;
    println!("  settled: {settled}");
    for round in 0..2 {
        let names = regions_of_one_query(&session, &file, cursor);
        println!("  settled round {round}: {names}");
    }

    // A second file's worth of work, so that the index is moving while the query runs — the state the whole probe
    // is about. `more.cpp` pulls `<map>` and `<memory>`, which is a new closure for the pump to read.
    let more = root.join("more.cpp");
    std::fs::write(&more, "#include <map>\n#include <memory>\nint more() { return 0; }\n")
        .expect("more.cpp writes");
    let queued = session.add_project_files([more.clone()]);
    println!("  queued {queued} more file(s)");

    for round in 0..6 {
        session.advance(INDEX_SLICE);
        let names = regions_of_one_query(&session, &file, cursor);
        println!(
            "  filling round {round}: pending {:<4} cooking {:<3} | {names}",
            session.pending(),
            session.pending_cooking()
        );
        if session.pending() == 0 && session.pending_cooking() == 0 {
            break;
        }
    }

    // ---------------------------------------------------------------------------------------------
    // 4. Are the two visibility walks the same answer?
    // ---------------------------------------------------------------------------------------------
    //
    // The one thing that can go wrong with the change this probe was written for, and the thing a duration
    // cannot show: the new walk is one traversal carrying macro state, the old one evaluated each condition
    // against a state built for its own file. A *different* set is a different completion list, which is a
    // regression wearing a speed-up's clothes.
    println!("\n--- the visibility set, both ways ---");
    let index = session.index();
    let new = index.visible_files(&file);
    let old = index.visible_files_by_asking_each_condition(&file);
    println!("  new: {} files | old: {} files", new.len(), old.len());

    let new_map: std::collections::BTreeMap<&str, &str> = new
        .iter()
        .map(|(path, visibility)| (path.as_str(), visibility_name(*visibility)))
        .collect();
    let old_map: std::collections::BTreeMap<&str, &str> = old
        .iter()
        .map(|(path, visibility)| (path.as_str(), visibility_name(*visibility)))
        .collect();

    let only_new: Vec<&str> = new_map
        .keys()
        .filter(|key| !old_map.contains_key(*key))
        .copied()
        .collect();
    let only_old: Vec<&str> = old_map
        .keys()
        .filter(|key| !new_map.contains_key(*key))
        .copied()
        .collect();
    let differing: Vec<(&str, &str, &str)> = new_map
        .iter()
        .filter_map(|(key, now)| {
            old_map
                .get(key)
                .filter(|before| *before != now)
                .map(|before| (*key, *before, *now))
        })
        .collect();

    println!("  only in the new walk: {}", only_new.len());
    for path in only_new.iter().take(10) {
        println!("    + {path}");
    }
    println!("  only in the old walk: {}", only_old.len());
    for path in only_old.iter().take(10) {
        println!("    - {path}");
    }
    println!("  same file, different visibility: {}", differing.len());
    for (path, before, now) in differing.iter().take(10) {
        println!("    ~ {path}: {before} -> {now}");
    }
    println!(
        "  {}",
        if only_new.is_empty() && only_old.is_empty() && differing.is_empty() {
            "IDENTICAL — the walk change is behaviour-preserving on this closure"
        } else {
            "DIFFERENT — see the three lists above"
        }
    );

    // ---------------------------------------------------------------------------------------------
    // 5. What one keystroke costs, which is the other half of what a user feels
    // ---------------------------------------------------------------------------------------------
    //
    // A completion is asked *after* a keystroke, so the number that matters is the pair: what the edit costs
    // (`did_change`, which every keystroke pays whether or not a popup is coming) and what the request then has to
    // do before its query can run (`view`, the parse; `catch_up`, the summary the edit dropped).
    println!("\n--- one keystroke, and then one completion ---");

    for round in 0..3 {
        let edited = format!("{MAIN_CPP}// round {round}\n");

        let editing = Instant::now();
        session.did_change(&file, &edited);
        let edit = editing.elapsed();

        let viewing = Instant::now();
        let view = session.view_of_the_file(&file);
        let viewed = viewing.elapsed();

        let catching = Instant::now();
        session.catch_up(&file);
        let caught = catching.elapsed();

        let querying = Instant::now();
        let items = view
            .map(|view| session.completions(&view, cursor).items.len())
            .unwrap_or(0);
        let queried = querying.elapsed();

        println!(
            "  keystroke {round}: did_change {} ms | view {} ms | catch_up {} ms | query {} ms ({items} items)",
            edit.as_millis(),
            viewed.as_millis(),
            caught.as_millis(),
            queried.as_millis()
        );
    }
    // ---------------------------------------------------------------------------------------------
    // 6. The classic hard cursor: a qualified name into a class hierarchy
    // ---------------------------------------------------------------------------------------------
    //
    // `std::ios::` and its relatives are the case every C++ completion is judged by: the scope names a **class**
    // with a base chain (`ios` -> `ios_base`), and what may be written after it is the class's own members *plus
    // every inherited one*. That is a different query from the name list the rounds above measure — it goes through
    // `names_in_a_scope` -> `names_of_a_class` -> `members_of`, which walks bases — so it is timed on its own
    // rather than assumed to follow from the numbers above.
    //
    // **The expression is injected as real code, inside `main`**, and that is not a detail: a cursor inside a
    // comment or a string is trivia, and the first version of this measured 0 ms and 0 items for every cursor
    // because of exactly that. `␟` marks where the cursor sits — a control character rather than `|`, because `|`
    // is itself C++ (`std::ios::|` has no bare one and a real `a | b` would have two).
    println!("\n--- the class-hierarchy cursors ---");
    const MARK: char = '\u{241F}';
    for (label, line) in [
        ("std::", "std::"),
        ("std::ios::", "std::ios::"),
        ("std::ostream::", "std::ostream::"),
        ("std::istream::", "std::istream::"),
        ("std::string::", "std::string::"),
        ("std::vector<int>::", "std::vector<int>::"),
        ("sux.", "sux."),
    ] {
        let at_the_cursor = format!("{line}{MARK}");
        let written = at_the_cursor.replace(MARK, "");
        let marker = at_the_cursor.find(MARK).expect("the marker is in the fixture");

        // One statement inside the body, before `return 0;`, so the cursor is a name position in a function.
        let source = MAIN_CPP.replace(
            "    return 0;\n}",
            &format!("    {written}\n    return 0;\n}}"),
        );

        session.did_change(&file, &source);
        session.catch_up(&file);
        let Some(view) = session.view_of_the_file(&file) else {
            continue;
        };

        let Some(at) = view.source.find(&written) else {
            eprintln!("  {label}: the injected line {written:?} is not in the view");
            continue;
        };
        let cursor = at + marker;

        let asking = Instant::now();
        let found = session.completions(&view, cursor);
        let took = asking.elapsed();

        let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
        println!(
            "  {label:<20} {:>5} ms | {:>4} item(s) | scope {:?}",
            took.as_millis(),
            found.items.len(),
            found.scope
        );
        println!("        {:?}", labels.iter().take(12).collect::<Vec<_>>());

        // Whether the answer moved session state, which is the difference between a first ask and a repeat.
        let again = Instant::now();
        let second = session.completions(&view, cursor);
        println!(
            "        again: {} ms, {} item(s)",
            again.elapsed().as_millis(),
            second.items.len()
        );
    }
}

fn visibility_name(visibility: cpp_code_analysis::IncludeVisibility) -> &'static str {
    match visibility {
        cpp_code_analysis::IncludeVisibility::Unconditional => "unconditional",
        cpp_code_analysis::IncludeVisibility::Conditional => "conditional",
    }
}

/// One query, with the trace window reset around it, reported as the crate's own regions.
fn regions_of_one_query(session: &Session<DiskFiles>, file: &Path, cursor: usize) -> String {
    let Some(view) = session.view_of_the_file(file) else {
        return "no view".to_string();
    };
    cpp_code_analysis::query_trace::reset();
    let _ = session.completions(&view, cursor);
    cpp_code_analysis::query_trace::report()
}

/// One completion, asked the way the handler asks it: the file's own view, then the query at an offset in it.
fn time_completion(session: &Session<DiskFiles>, file: &Path, offset: usize) -> Duration {
    let started = Instant::now();
    let Some(view) = session.view_of_the_file(file) else {
        return started.elapsed();
    };
    let _ = session.completions(&view, offset);
    started.elapsed()
}

/// Just the view — the file's parse, which the handler measures as `view N ms`.
fn time_view(session: &Session<DiskFiles>, file: &Path) -> Duration {
    let started = Instant::now();
    let _ = session.view_of_the_file(file);
    started.elapsed()
}

fn offset_in(source: &str, needle: &str) -> Option<usize> {
    source.find(needle)
}

fn position_of(source: &str, needle: &str) -> (u32, u32) {
    let at = source.find(needle).expect("the needle is in the fixture");
    let before = &source[..at];
    let line = before.matches('\n').count() as u32;
    let character = before
        .rsplit('\n')
        .next()
        .map(|last| last.len())
        .unwrap_or(0) as u32;
    (line, character)
}
