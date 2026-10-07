//! **What one keystroke costs, in passes.**
//!
//! `docs/incremental-edits.md` §1 counts the passes a single character causes — three parses, three lexes, a line
//! index, and a walk of the include closure — and §5.2's acceptance is stated as a count rather than a duration:
//! *one* `Parse` entry for the edited file. This is the probe that reads the counts, and it is written as an
//! example rather than a test because the counters in [`cpp_code_analysis::stages`] are **process-global**: a test
//! binary running forty tests in parallel would report the sum of all of them.
//!
//! ```text
//!   cargo run --release -p cpp_code_analysis --example keystroke
//! ```
//!
//! It reports three edits, because they are three different amounts of work and the difference is the design:
//!
//! ```text
//!   a character in a body       what a person does thousands of times an hour, and the one that must be cheap
//!   a directive added below     the same keystroke with a `#define`, which *is* an input to the file's timeline
//!   an include added above      which moves the file's preamble, and is the case a unit cache must refuse
//! ```
//!
//! # What it does not measure
//!
//! The server's own path: `cpp_ls` adds a request, a lock and a pump around this, and `crates/cpp_ls/tests/latency.rs`
//! is what measures that end. What is here is the *analysis* half — how many times one file is read, and how far
//! past it the work reaches — which is the half that a lock cannot fix.

use std::path::PathBuf;

use cpp_code_analysis::stages::{Stage, StageTimes};
use cpp_code_analysis::{
    CompilerConfig, FileProvider, OpenDocuments, Session, SessionFiles, UnitStats, WatchFilter,
};
use cpp_code_analysis::file::paths::MemoryFiles;

/// The stages that answer "how many passes did this take", in the order a keystroke reaches them.
const PASSES: &[Stage] = &[
    Stage::Read,
    Stage::Hash,
    Stage::Lookup,
    Stage::IncludeScan,
    Stage::Load,
    Stage::Parse,
    Stage::Sweep,
    Stage::Drop,
    Stage::Lex,
    Stage::Macros,
    Stage::Render,
    Stage::RenderParse,
    Stage::RenderSweep,
    Stage::Map,
    Stage::BodiedScan,
    Stage::ReFilter,
    Stage::IndexInsert,
    Stage::Walk,
    Stage::Closure,
    Stage::UnitGet,
    Stage::UnitPut,
];

fn main() {
    let files = fixture();
    let root = std::env::temp_dir().join("cppls-keystroke-probe");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe's cache directory");

    let documents = OpenDocuments::new();
    let providers = SessionFiles::new(documents, files.clone());
    let mut session = Session::with_config(
        &root,
        providers,
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );

    let main = files.read(&PathBuf::from("/p/main.cpp")).expect("the fixture");
    session.did_open("/p/main.cpp", &main);
    session.add_project_files([PathBuf::from("/p/main.cpp")]);
    session.index_everything();

    println!("the fixture: {} file(s) in the closure\n", session.index().len());

    // **A character in a body** — the keystroke the whole document is about.
    let typed = format!("{main}\nint more() {{ return WIDTH; }}\n");
    report(&mut session, "a character in a body", &typed);

    // **A `#define` below the body.** It is not in the preamble, so the unit is still the unit — and it is a
    // directive, so the file's own summary has to be rebuilt with it.
    let with_a_define = format!("{main}\n#define LATE 2\nint more() {{ return LATE; }}\n");
    report(&mut session, "a `#define` below the body", &with_a_define);

    // **An `#include` above the body** — the case the unit cache must refuse.
    let with_an_include = format!("#include \"extra.h\"\n{main}");
    report(&mut session, "an `#include` above the body", &with_an_include);
}

/// Run one edit, and say what it cost.
fn report(session: &mut Session<MemoryFiles>, what: &str, text: &str) {
    let before = StageTimes::read();
    let entries_before: Vec<u64> = PASSES.iter().map(|stage| StageTimes::entries(*stage)).collect();
    let units_before = session.unit_stats();

    session.did_change("/p/main.cpp", text);
    session.index_everything();

    let spent = StageTimes::read().since(before);
    let units = session.unit_stats();

    println!("--- {what} ---");
    println!("  {:<14}  {:>6}  {:>12}", "stage", "passes", "time");
    for (stage, was) in PASSES.iter().zip(&entries_before) {
        let now = StageTimes::entries(*stage);
        let took = spent.of(*stage);
        // Only the stages this edit actually entered: a table of zeroes is not a measurement.
        if now > *was {
            println!(
                "  {:<14}  {:>6}  {:>9.2} ms",
                stage.name(),
                now - was,
                took.as_secs_f64() * 1000.0
            );
        }
    }
    println!("  {:<14}  {:>6}  {:>12}", "…units", "", human(units, units_before));

    // **The acceptance line**, in the words §5.2 states it in: how many times the edited file was parsed, and
    // whether it was macro-expanded at all.
    let passes = |stage: Stage| StageTimes::entries(stage) - entries_before[at(stage)];
    println!(
        "  => parsed {} time(s), lexed {}, rendered {}, rendering parsed {}",
        passes(Stage::Parse),
        passes(Stage::Lex),
        passes(Stage::Render),
        passes(Stage::RenderParse),
    );
    println!();
}

/// Where a stage sits in [`PASSES`], so the "before" reading can be subtracted.
fn at(stage: Stage) -> usize {
    PASSES.iter().position(|held| *held == stage).expect("a stage the probe reports on")
}

/// The unit counters as a change, so that "walked" is readable as *this edit walked one* rather than as a total.
fn human(now: UnitStats, was: UnitStats) -> String {
    format!(
        "memory +{}, disk +{}, WALKED +{}",
        now.from_memory - was.from_memory,
        now.from_disk - was.from_disk,
        now.walked - was.walked
    )
}

/// A small project with a real shape: a `.cpp`, a header it includes which includes two more, and one at the end
/// of that chain — so the closure is four files and the walk has something to walk.
fn fixture() -> MemoryFiles {
    MemoryFiles::new()
        .with_file("/p/width.h", "#pragma once\n#define WIDTH 4\n")
        .with_file("/p/config.h", "#pragma once\n#include \"width.h\"\nstruct Cfg { int w; };\n")
        .with_file("/p/api.h", "#pragma once\n#include \"config.h\"\nstruct Api { Cfg c; };\n")
        .with_file("/p/extra.h", "#pragma once\nstruct Extra { int e; };\n")
        .with_file(
            "/p/main.cpp",
            "#include \"api.h\"\nint main() { Api a; return a.c.w; }\n",
        )
}
