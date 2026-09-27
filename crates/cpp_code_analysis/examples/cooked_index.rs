//! **Can the cooked reading be the parse the analysis layer works from?**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example cooked_index -- <file-list>
//! ```
//!
//! The question this answers is about **plumbing**, not about quality: a summary is built from a tree, and a tree is
//! built from text — so if the text handed to the summary is the **rendering** of a cooked stream, then every range
//! the summary carries points into the rendering rather than into the file, and the whole analysis layer above it
//! (facts, guards, includes, the index) is wrong in a way nothing would report. `RenderedCooked::written_span` is
//! what turns those ranges back, and this is the first consumer that needs it for **every fact** rather than for one
//! diagnostic.
//!
//! So per file it: summarises the file's own text, cooks and renders it, summarises the **rendering**, and compares
//!
//! * the **declaration names** the two readings find — a name is what the layer above actually consumes, and a set
//!   difference is visible where a count is not;
//! * how many of the cooked summary's fact ranges **map back into the file** at all.
//!
//! The cooked path here is **level 0** — this probe has no include closure, so most macros are unknown and the
//! rendering is mostly the file's own tokens. That is deliberate: what is being tested is that the plumbing holds
//! when the two texts differ, and `std_probe --cooked` is where the *quality* of a cooked reading is measured.
//!
//! # What it measured (128 files, libstdc++, level 0)
//!
//! ```text
//! files 128 | same declaration names 42
//! names only in the raw reading 2595 | only in the cooked reading 1516
//! cooked fact ranges mapped back into the file 9847 | not mapped 0
//! ```
//!
//! **The plumbing holds**: every one of the 9847 fact ranges a rendering-based summary produced maps back into the
//! file through `written_span`. That was the question.
//!
//! **The readings differ, and by design.** The raw reading indexes what the file *says* — both branches of every
//! `#if`, and the declarations inside macro bodies — while the cooked reading indexes what a compiler would *see*:
//! one branch, macros expanded, directives gone. So a summary from a rendering is **not** a drop-in replacement for
//! one from the file, and this is the number that says so. Which is the architecture's own split (§2.5 and §4, the
//! function-assignment table), measured rather than assumed: the raw side answers "what is written here", the
//! cooked side answers "what is compiled".

use std::collections::BTreeSet;
use std::path::Path;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(list) = arguments.next() else {
        eprintln!("usage: cooked_index <file-list>");
        std::process::exit(2);
    };

    let text = std::fs::read_to_string(&list).expect("the list");
    let mut files = 0usize;
    let mut identical = 0usize;
    let mut missing = 0usize;
    let mut extra = 0usize;
    let mut mapped = 0usize;
    let mut unmapped = 0usize;
    let mut shown = 0usize;

    for path in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        files += 1;
        let path = Path::new(path);

        let raw = cpp_code_analysis::summarize(path, &source, cpp_code_analysis::SummaryKey::new(0, 0));

        let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
        let cooked = cpp_code_analysis::cook(&source, &tokens);
        let rendered = cooked.render();
        let from_cooked = cpp_code_analysis::summarize(path, &rendered.text, cpp_code_analysis::SummaryKey::new(0, 0));

        let names = |summary: &cpp_code_analysis::FileSummary| -> BTreeSet<String> {
            summary
                .declarations
                .iter()
                .map(|fact| fact.name.clone())
                .collect()
        };
        let raw_names = names(&raw);
        let cooked_names = names(&from_cooked);

        let lost = raw_names.difference(&cooked_names).count();
        let gained = cooked_names.difference(&raw_names).count();
        missing += lost;
        extra += gained;
        if raw_names == cooked_names {
            identical += 1;
        } else if shown < 12 {
            shown += 1;
            println!("{}", path.display());
            for name in raw_names.difference(&cooked_names).take(4) {
                println!("   only in the raw reading: {name}");
            }
            for name in cooked_names.difference(&raw_names).take(4) {
                println!("   only in the cooked reading: {name}");
            }
        }

        for fact in &from_cooked.declarations {
            if rendered.written_span(fact.range).is_some() {
                mapped += 1;
            } else {
                unmapped += 1;
            }
        }
    }

    println!("\nfiles {files} | same declaration names {identical}");
    println!("names only in the raw reading {missing} | only in the cooked reading {extra}");
    println!("cooked fact ranges mapped back into the file {mapped} | not mapped {unmapped}");
}
