//! **What `symbol_references` costs, rung by rung, and what the use filter is worth** — the symbol-path twin of
//! `find_references.rs`, which measures the macro path.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example symbol_references_cost -- <dir> [<entry.cpp>]
//! ```
//!
//! # Why this exists
//!
//! The macro-path probe says, on this closure, that **reading is not the cost**: 459 files and 10.4 MB in 26 ms,
//! against whole queries of 124 ms to 3.2 s whose gap to the lexing rung is a per-hit check. The symbol path has no
//! per-hit check — it collects the identifiers of each candidate — so its cost had to be measured rather than
//! inherited from the macro measurement.
//!
//! The question this answers is narrow and it is the one a 74 808-byte field has to justify:
//!
//! ```text
//!   does the use filter (`FileSummary::use_filter`) skip a meaningful share of the candidates,
//!   and is the read it skips a meaningful share of the query?
//! ```
//!
//! # The rungs
//!
//! ```text
//!  1. candidates        the declaring file and everything that transitively includes it
//!  2a. the filter       how many of them the summary's Bloom filter rejects with no read at all
//!  2b. read             the whole file, per candidate — what the filter removes
//!  2c. contains         the substring scan, exact, per candidate that was read
//!  3. lex               `identifiers()` over the survivors
//!  4. the query         what the caller pays end to end
//! ```
//!
//! A rung that dominates is where the next change goes, and the answer is not assumed: the macro probe's answer to
//! the same question was "not the read", and that is why this one exists.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{
    DiskFiles, FileProvider, Known, OpenDocuments, Session, SessionFiles, SymbolToFind, WatchFilter,
    symbol_references,
};
use cpp_code_analysis::index::references::ReferenceBudget;

/// The names asked about. Chosen for being declared in headers that a standard-library closure includes widely, so
/// that the candidate set is large — which is the case an index has to pay for. They are not picked by taste: the
/// probe prints how many files declare each one and how many candidates it produced, and a name with a small
/// candidate set answers nothing.
const NAMES: &[&str] = &["size", "begin", "push_back", "value_type", "operator=", "data"];

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: symbol_references_cost <dir> [<entry.cpp>]");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = std::env::args().nth(2).map(PathBuf::from);

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    if let Some(entry) = &entry {
        let text = std::fs::read_to_string(entry).unwrap_or_default();
        session.did_open(entry, &text);
    }
    let started = Instant::now();
    session.index_everything();
    println!(
        "project: {} files indexed in {:?}",
        session.index().len(),
        started.elapsed()
    );

    let files = DiskFiles;

    for name in NAMES {
        // **A declaration to ask about**, taken from the index the same way a cursor's would be resolved: the first
        // non-local declaration of that name the index holds. A name nothing declares is skipped rather than made up.
        let Some((fact, declared_in)) = session.index().summaries().find_map(|summary| {
            summary
                .declarations
                .iter()
                .find(|fact| fact.name == *name && !fact.local)
                .map(|fact| (fact.clone(), summary.path.clone()))
        }) else {
            println!("\n{name}\n  nothing in this project declares it");
            continue;
        };

        let symbol = SymbolToFind::of(&fact, declared_in.clone());
        let started = Instant::now();
        let answer = symbol_references(session.index(), &files, &symbol, ReferenceBudget::default());
        let query = started.elapsed();

        let Known::Yes(found) = &answer else {
            println!("\n{name}\n  {answer:?}");
            continue;
        };

        // **What the filter alone rejects**, asked here rather than reported by the query: the query counts a filter
        // rejection and a `contains` rejection in the same counter (`without_the_name`), and the whole point of this
        // probe is to tell them apart.
        let candidates: HashSet<PathBuf> = found
            .files
            .iter()
            .map(|file| file.file.clone())
            .chain(found.unreadable.iter().cloned())
            .collect();
        let mut rejected_by_the_filter = 0usize;
        let mut read_bytes = 0usize;
        let started = Instant::now();
        for file in session.index().summaries().map(|summary| &summary.path) {
            if !candidates.contains(file) {
                continue;
            }
            match session.index().summary(file) {
                Some(summary)
                    if !cpp_code_analysis::summary::use_filter_might_contain(
                        &summary.use_filter,
                        &symbol.last_segment,
                    ) =>
                {
                    rejected_by_the_filter += 1;
                }
                _ => {
                    read_bytes += files.read(file).map(|text| text.len()).unwrap_or(0);
                }
            }
        }
        let read = started.elapsed();

        println!("\n{name}   (declared in {})", declared_in.display());
        println!("  1. candidates                 {}", found.looked_at + found.without_the_name);
        println!("  2a. rejected by the filter    {rejected_by_the_filter}");
        println!(
            "  2b. read                      {} file(s), {} KB in {read:?}",
            found.looked_at,
            read_bytes / 1024
        );
        println!("  2c. without the name          {}", found.without_the_name);
        println!("  3+4. the whole query          {query:?} — {} file(s) in the answer", found.files.len());
    }
}
