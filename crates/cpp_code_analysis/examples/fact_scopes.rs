//! How a set of files' declaration facts are distributed across scopes — and **how fast a fact-level question can be
//! answered**, which is the reason this probe exists.
//!
//! It parses each file on its own, so it needs no include closure and finishes in about ten seconds over ninety
//! headers, against the five minutes `types_probe` takes for eight. Any question that can be asked of a **single
//! file's facts** should be asked here.
//!
//! # What it measured once, and why the answer is in this comment
//!
//! This probe was written to size a proposed third state of [`DeclFact::scope`] — "the file's text opens a scope the
//! reading cannot name", which is what a namespace opened by a **macro** (`_STD_BEGIN` is `namespace std {`) produces
//! in a reading that does not expand macros. The case for it was `std::endl`: `__msvc_ostream.hpp` files `endl` with
//! `scope: None`, and a qualified lookup matches on the qualified name or an exact bare one, so `std::endl` found
//! nothing.
//!
//! The mechanism was built, and this probe is what killed it: over 93 MSVC headers and **34070 facts**, the state
//! fired **zero times**. The reason is one line of the design — a file's macro table holds the macros **that file**
//! defines, and `_STD_BEGIN` is defined in another header — so the signal could never fire where it was needed, and
//! `std::endl` had in fact been answered by the **cooked reading**, which does expand macros and files the name under
//! `std`. A test that passed, on a fixture whose macro was in the same file, is what made it look otherwise.
//!
//! So the distribution below is what the probe is for, and the zero it once reported is kept here rather than in a
//! commit nobody will read.
//!
//! ```text
//! usage: fact_scopes <file-list>
//! ```
use std::path::PathBuf;

fn main() {
    let list = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: fact_scopes <file-list>");
        std::process::exit(2);
    });

    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list is readable")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let mut facts = 0usize;
    let mut at_file_scope = 0usize;
    let mut in_a_known_scope = 0usize;
    let mut locals = 0usize;
    let mut with_a_type = 0usize;

    for path in &paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };

        let summary = cpp_code_analysis::index::summarize(
            path,
            &source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );

        for fact in &summary.declarations {
            facts += 1;
            if fact.type_of.is_some() {
                with_a_type += 1;
            }
            if fact.local {
                locals += 1;
            } else if fact.scope.is_some() {
                in_a_known_scope += 1;
            } else {
                at_file_scope += 1;
            }
        }
    }

    println!("{} file(s)", paths.len());
    println!("{facts} declaration facts");
    println!("  {in_a_known_scope} in a scope the text spells out");
    println!("  {at_file_scope} at file scope");
    println!("  {locals} local, which is a separate field");
    println!("  {with_a_type} carry a type");
}
