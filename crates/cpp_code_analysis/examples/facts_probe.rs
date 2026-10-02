//! Read one file the way the indexer reads it, and print the facts one name produced.
//!
//! ```text
//! facts_probe <file> <name> [<name> …]
//! ```
//!
//! # Why this exists
//!
//! A name that cannot be found can be missing for two reasons that call for completely different work: the
//! index holds the declaration and the lookup cannot reach it, or the declaration never became a fact at all.
//! Answering that from a whole session costs a cook, an index and a probe that knows how to ask — and the
//! answer is one line: **what does the summary hold under this name**.
//!
//! This reads the file with the same call the indexer makes ([`cpp_code_analysis::summarize`]), so what it
//! prints is what the index would hold, with the offsets and the base clauses that decide every question
//! downstream. It parses no includes and expands no macros: the input is the text to be read, and the point is
//! to be able to hand it the **rendering** of a real header when that is what the analysis actually reads.
//!
//! ```text
//! facts_probe target/scratch/xm1/tu.cpp allocator_traits
//! ```

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: facts_probe <file> <name> [<name> …]");
        std::process::exit(2);
    };
    let wanted: Vec<String> = args.collect();

    let source = std::fs::read_to_string(&path).expect("the file is readable");
    let summary = cpp_code_analysis::summarize(
        std::path::Path::new(&path),
        &source,
        cpp_code_analysis::SummaryKey::new(0, 0),
    );

    println!(
        "{}: {} bytes, {} declarations, {} macros, {} includes",
        path,
        source.len(),
        summary.declarations.len(),
        summary.macros.len(),
        summary.includes.len()
    );

    if wanted.is_empty() {
        // No name asked for: the shape of the reading, which is what says whether a name is missing from a
        // hole the reader left or from a lookup that cannot reach it.
        let highest = summary
            .declarations
            .iter()
            .map(|fact| fact.range.end_offset())
            .max()
            .unwrap_or(0);
        println!("   highest declaration offset: {highest} of {} bytes", source.len());
        return;
    }

    for name in &wanted {
        let found: Vec<&cpp_code_analysis::DeclFact> = summary
            .declarations
            .iter()
            .filter(|fact| &fact.name == name || fact.qualified_name() == *name)
            .collect();

        println!("`{name}`: {} fact(s)", found.len());
        for fact in found {
            println!(
                "   @{:<7} {:<44} kind {:?} bases {:?} type_of {:?}",
                fact.range.start_offset,
                fact.qualified_name(),
                fact.kind,
                fact.bases,
                fact.type_of
            );
        }
    }
}
