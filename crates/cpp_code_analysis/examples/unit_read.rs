//! What reading a directory's first translation unit **as a program** does to the answers.
//!
//! ```text
//! cargo run --release --example unit_read -- <dir> [<file.cpp>]
//! ```
//!
//! Opens a session, drains it, prints the readings the per-file policy gives, then reads the unit as one program
//! ([`Session::read_the_unit`]) and prints them again — with the report the read itself makes (files quarantined,
//! braces still paired across files, errors). The three readings that made the unit read a loss before scopes were
//! fenced are here: `declarations_in("std")`, and where `std::size_t` and `std::string` resolve.

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};
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
}
