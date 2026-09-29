//! What reading a directory's first translation unit **as a program** does to the answers.
//!
//! ```text
//! cargo run --release --example unit_read -- <dir> [<file.cpp>]
//! ```
//!
//! Opens a session, drains it, prints the readings the per-file policy gives, then reads the unit as one program
//! ([`Session::read_the_unit`]) and prints them again — with the report the read itself makes (files quarantined,
//! braces still paired across files, errors).
//!
//! # What it decides
//!
//! Whether the unit read may be **wired into the pump** — `Session::advance` calls it only if the answers hold up,
//! and the switch is off while they do not. So the readings are the ones that moved when it was tried:
//!
//! * `declarations_in("std")` — the count that **fell by 49** before the crossing fence existed;
//! * `std::size_t` and `std::string` — one name goes from "not declared here" to `Ambiguous` (both are "unknown",
//!   and they are not the same unknown), and the other must keep resolving in `<xstring>`;
//! * **every identifier of the file, through `definition`** — the census, because a total can hide a file that
//!   gained and a file that lost: what a unit read adds is a *cooked* reading for every file in the program, and
//!   the answer for one name is a list where it used to be a single declaration;
//! * the cost, per stage — a unit read is one walk, one render and one parse of the whole program.
//!
//! The census is printed **twice**, before and after, in one process: a difference between two runs is a
//! difference between two machines, and every question here is a difference.

use cpp_code_analysis::{
    DiskFiles, Known, OpenDocuments, Session, SessionFiles, UnknownReason, WatchFilter,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    let readings = |session: &Session<DiskFiles>, label: &str| {
        let in_std = session.index().declarations_in("std", &file).len();
        let size_t = match session.index().definition("std::size_t", &file) {
            Known::Yes(found) => format!("in {}", found.file.display()),
            other => format!("{other:?}").chars().take(40).collect(),
        };
        let string = match session.index().definition("std::string", &file) {
            Known::Yes(found) => format!("in {}", found.file.display()),
            other => format!("{other:?}").chars().take(40).collect(),
        };
        println!("{label:<10} declarations_in(\"std\") = {in_std:>5} | std::size_t {size_t} | std::string {string}");
    };

    // **Every identifier of the file, by what `definition` answers for it.** `Ambiguous` is listed by name because
    // it is the reason to keep the switch off: a name with two candidates where there used to be one is not a
    // number, it is a question a hover has to answer differently.
    let census = |session: &Session<DiskFiles>, label: &str| {
        let Some(view) = session.view(&file) else {
            println!("{label:<10} (the file is not held)");
            return;
        };
        let mut outcomes: HashMap<String, usize> = HashMap::new();
        let mut ambiguous: HashMap<String, usize> = HashMap::new();
        for token in view.tree.get_tokens() {
            if token.kind != cpp_parser::CppTokenKind::Identifier {
                continue;
            }
            let offset = token.range.start_offset;
            let reason = match session.definition(&view, offset) {
                Known::Yes(found) if found.file == file => "resolved here".to_string(),
                Known::Yes(_) => "resolved in a header".to_string(),
                Known::Unknown(UnknownReason::NotDeclaredHere(_)) => "the index has no such name".to_string(),
                Known::Unknown(reason) => {
                    if let UnknownReason::Ambiguous(name) = &reason {
                        *ambiguous.entry(name.to_string()).or_default() += 1;
                    }
                    format!("unknown: {reason:?}")
                }
                Known::No => "no name at this offset".to_string(),
            };
            *outcomes.entry(reason).or_default() += 1;
        }

        println!("{label:<10} definition, over every identifier:");
        let mut ranked: Vec<(String, usize)> = outcomes.into_iter().collect();
        ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (reason, count) in ranked {
            println!("             {count:>5}  {reason}");
        }
        if !ambiguous.is_empty() {
            let mut names: Vec<(String, usize)> = ambiguous.into_iter().collect();
            names.sort_by(|one, other| other.1.cmp(&one.1).then_with(|| one.0.cmp(&other.0)));
            let listed: Vec<String> = names.iter().map(|(name, count)| format!("{name}×{count}")).collect();
            println!("             still ambiguous: {}", listed.join(", "));
        }
    };

    census(&session, "per file");
    readings(&session, "per file");

    cpp_code_analysis::stages::StageTimes::reset();
    let started = Instant::now();
    let reading = session.read_the_unit(&file);
    let elapsed = started.elapsed();
    print!("{}", cpp_code_analysis::stages::StageTimes::read().report());

    match reading {
        Some(reading) => {
            println!(
                "unit read in {elapsed:.2?}: {} files filed, {} tokens ({} files with tokens), {} missing, {} unplaced",
                reading.files, reading.tokens, reading.files_with_tokens, reading.missing, reading.unplaced
            );
            println!(
                "  errors {} | crossings {} | quarantined {:?} | unbalanced {:?} | braces {}",
                reading.errors, reading.crossings, reading.quarantined, reading.unbalanced, reading.braces
            );
        }
        None => println!("the unit could not be read (no summary for {})", file.display()),
    }

    readings(&session, "with unit");
    census(&session, "with unit");
}
