//! What the server sees in *this* directory: the toolchain it discovers, the files it indexes, and — for every
//! identifier and every offset in a file — **which question fails**.
//!
//! ```text
//! cargo run --release --example workspace_probe -- <dir> [<file.cpp>]
//! ```

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

    let started = Instant::now();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let opening = started.elapsed();

    let indexed = Instant::now();
    session.index_everything();
    println!(
        "opened in {opening:?} | toolchain {:?} | {} include paths | indexed {} files in {:?} (pending {})",
        session.toolchain().and_then(|toolchain| toolchain.version.clone()),
        session.config().include_paths.len(),
        session.project_files().len(),
        indexed.elapsed(),
        session.pending()
    );

    // **What the index actually holds**, asked directly: whether the facts are there at all (an alias or a
    // template may never become one) or whether the lookup is keyed differently.
    let index = session.index();
    for name in [
        "std::string", "string", "std::optional", "optional", "std::cin", "std::getline",
        "std::size_t", "size_t", "std::cout",
    ] {
        println!("index.definition({name:?}) = {:?}", index.definition(name, &file).value().map(|found| (found.fact.qualified_name(), found.fact.kind)));
    }
    println!(
        "declarations_in(\"std\", main.cpp) = {}, project files {}",
        index.declarations_in("std", &file).len(),
        session.project_files().len()
    );
    for header in ["string", "optional", "iostream"] {
        let path = std::path::PathBuf::from(format!(
            "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.35.32215\\include\\{header}"
        ));
        println!(
            "  {header}: indexed {} | cooked {} | summary declarations {:?}",
            session.view(&path).is_some(),
            session.is_indexed(&path),
            session.index().summary(&path).map(|summary| summary.declarations.len())
        );
    }
    // **Is anything cooked at all?** A session cooks the *open* files' closures, and a probe that never opened
    // the file has cooked nothing — so `definition("std::string")` above may have been asked of a half-built
    // index. This is the difference between "the facts are unscoped" and "the scoped facts were never made".
    println!(
        "before opening: pending {} cooking {}",
        session.pending(),
        session.pending_cooking()
    );
    let opened = std::fs::read_to_string(&file).unwrap_or_default();
    session.did_open(&file, &opened);
    let cooked = session.cook_the_open_files();
    println!(
        "after opening: cooked {} file(s), pending {} cooking {}",
        cooked.len(),
        session.pending(),
        session.pending_cooking()
    );

    // **Drain the pump.** Cooking is *queued*, not done: `cook_the_open_files` hands the closure to the queue and
    // the session's own slice-by-slice drain is what actually reads it. A probe that stopped here would be
    // measuring an index with no cooked reading at all — which is exactly the mistake this line fixes.
    let draining = Instant::now();
    let mut rounds = 0usize;
    while (session.pending() > 0 || session.pending_cooking() > 0) && rounds < 10_000 {
        session.advance(64);
        rounds += 1;
    }
    println!(
        "drained in {draining:?} ({rounds} rounds): pending {} cooking {}",
        session.pending(),
        session.pending_cooking()
    );
    // **What the cooked reading added**, which is the question the raw summary cannot answer: if the scope of a
    // declaration inside a macro-opened namespace is visible anywhere, it is here.
    let in_std = session.index().declarations_in("std", &file);
    println!("after draining, declarations_in(\"std\") = {}", in_std.len());
    for declaration in in_std.iter().take(10) {
        println!(
            "   {} | scope={:?} kind={:?} file={}",
            declaration.fact.qualified_name(),
            declaration.fact.scope,
            declaration.fact.kind,
            declaration.file.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    for name in ["std::basic_string", "basic_string", "std::char_traits", "std::allocator"] {
        println!(
            "   definition({name:?}) = {:?}",
            session.index().definition(name, &file).value().map(|found| found.fact.qualified_name())
        );
    }
    for name in ["std::string", "string", "std::optional", "std::cin", "std::getline"] {
        println!(
            "  after cooking: index.definition({name:?}) = {:?}",
            session
                .index()
                .definition(name, &file)
                .value()
                .map(|found| (found.fact.qualified_name(), found.fact.kind))
        );
    }
    let string_header = std::path::PathBuf::from(
        "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\14.35.32215\\include\\string",
    );
    if let Some(summary) = session.index().summary(&string_header) {
        println!("--- what <string>''s {} declarations are called ---", summary.declarations.len());
        for fact in summary.declarations.iter().take(400) {
            if matches!(fact.name.as_str(), "string" | "basic_string" | "allocator" | "char_traits" | "size_t") {
                println!("   name={:<16} qualified={:<28} scope={:?} kind={:?}", fact.name, fact.qualified_name(), fact.scope, fact.kind);
            }
        }
        let scoped = summary.declarations.iter().filter(|fact| fact.scope.is_some()).count();
        println!("   of {} declarations, {scoped} carry a scope", summary.declarations.len());
    }

    // **The cooked reading of one STL header, asked for directly.** This is the measurement that decides the fix:
    // if the rendering's summary carries the scope, the two readings disagree and the lookup is the broken half;
    // if it does not, the rendering (or the parse of it) is.
    match session.cook(&string_header) {
        Some(reading) => println!(
            "cook(<string>): {} declarations, {} of them only after expansion, {} diagnostics, {} unplaced, {} ranges mapped",
            reading.declarations,
            reading.only_after_expansion,
            reading.diagnostics,
            reading.unplaced,
            reading.mapped.placed
        ),
        None => println!("cook(<string>) answered nothing"),
    }
    println!(
        "   and the index now finds {} declarations in `std`, {} of them from the cooked reading",
        session.index().declarations_in("std", &file).len(),
        session.index().declarations_in("std", &file).iter().filter(|found| found.file == string_header).count()
    );
    let Some(view) = session.view(&file) else {
        println!("{} is not held", file.display());
        return;
    };
    let source = view.source.to_string();
    let line_of = |offset: usize| source[..offset].matches('\n').count();

    let mut outcomes: HashMap<String, usize> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();

    for token in view.tree.get_tokens() {
        if token.kind != cpp_parser::CppTokenKind::Identifier {
            continue;
        }
        let offset = token.range.start_offset;
        let name = &source[token.range.start_offset..token.range.end_offset()];
        let line = line_of(offset) + 1;
        let text = source.lines().nth(line - 1).unwrap_or_default().trim_end().to_string();

        match session.definition(&view, offset) {
            Known::Yes(found) => {
                let where_from = if found.file == file { "resolved here" } else { "resolved in a header" };
                *outcomes.entry(where_from.to_string()).or_default() += 1;
            }
            Known::Unknown(UnknownReason::NotDeclaredHere(_)) => {
                *outcomes.entry("the index has no such name".to_string()).or_default() += 1;
                failures.push(format!("{line:>4}  {name:<26} the index has no such name  | {}", &text[..text.len().min(50)]));
            }
            Known::Unknown(reason) => {
                *outcomes.entry(format!("unknown: {reason:?}")).or_default() += 1;
            }
            Known::No => {
                *outcomes.entry("no name at this offset".to_string()).or_default() += 1;
            }
        }
    }

    println!("\n--- definition, over every identifier ---");
    let mut ranked: Vec<(String, usize)> = outcomes.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (outcome, count) in &ranked {
        println!("{count:6}  {outcome}");
    }
    println!("--- unresolved, on code ({}) ---", failures.len());
    for failure in failures.iter().take(30) {
        println!("{failure}");
    }

    let mut refused: Vec<usize> = Vec::new();
    let mut offered_at_all = 0usize;
    for offset in 0..source.len() {
        match session.name_completions(&view, offset) {
            Known::Yes(_) => offered_at_all += 1,
            _ => {
                if session.member_completions(&view, offset).value().is_none() {
                    refused.push(offset);
                }
            }
        }
    }

    println!("\n--- completion, over every offset ---");
    println!(
        "{offered_at_all} of {} offsets answer, {} refuse; the ones in code:",
        source.len(),
        refused.len()
    );
    for offset in refused
        .iter()
        .filter(|offset| {
            let line = source[..**offset].lines().last().unwrap_or_default().trim_start();
            !line.starts_with("//") && !line.starts_with("R\"")
        })
        .take(24)
    {
        let line = line_of(*offset);
        let text = source.lines().nth(line).unwrap_or_default();
        let column = offset - source[..*offset].rfind('\n').map_or(0, |at| at + 1);
        println!("{:>4}:{:<3} | {}", line + 1, column, &text[..text.len().min(60)]);
    }
}
