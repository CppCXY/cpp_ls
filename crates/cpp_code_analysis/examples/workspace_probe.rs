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

    // **The two halves of the work, timed apart.** `index_everything` is one number, and the two things it does are
    // different jobs with different fixes: reading a file **into the index** (parse + scopes + facts, one file at a
    // time) and **cooking** it (a walk of the unit as a compiler would read it, then a parse of the rendering).
    // The first is bounded by the project; the second is bounded by the closure, and on a standard-library project
    // that is where the time goes.
    let indexed = Instant::now();
    cpp_code_analysis::stages::StageTimes::reset();
    while session.pending() > 0 {
        session.advance(64);
    }
    let indexed_for = indexed.elapsed();

    let cooked = Instant::now();
    while session.pending_cooking() > 0 {
        session.advance(64);
    }
    let cooked_for = cooked.elapsed();

    println!(
        "opened in {opening:?} | toolchain {:?} | {} include paths | {} project files | indexed {} files in {indexed_for:?} + cooked {cooked_for:?} (pending {} | stats {:?})",
        session.toolchain().and_then(|toolchain| toolchain.version.clone()),
        session.config().include_paths.len(),
        session.project_files().len(),
        session.index().len(),
        session.pending(),
        session.stats()
    );

    // **Where the time went**, stage by stage — and how much of the wall clock that accounts for.
    //
    // The gap is the point of printing both: a table that covers most of the wall time is an instrument, and one
    // that covers a third is telling us where to look next. `stages::StageTimes` documents the rule that makes the
    // sum meaningful (the stages do not overlap).
    let stages = cpp_code_analysis::stages::StageTimes::read();
    let wall = (indexed_for + cooked_for).as_secs_f64() * 1000.0;
    println!(
        "stages account for {:.1} ms of the {:.1} ms of indexing + cooking",
        stages.total().as_secs_f64() * 1000.0,
        wall
    );
    print!("{}", stages.report());

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
    // **The standard library's own directory, asked of the session rather than written down.** A probe that spells
    // out `…\MSVC\14.35.32215\include` is a probe that stops finding anything the day the machine's toolset is
    // updated — which is the same mistake as hard-coding a version into the analysis, one layer up.
    let standard_library = session
        .config()
        .system_include_paths()
        .next()
        .map(std::path::Path::to_path_buf);
    if let Some(directory) = &standard_library {
        println!("the standard library's directory: {}", directory.display());
    }

    for header in ["string", "optional", "iostream", "istream", "ostream", "xstring"] {
        let Some(path) = standard_library.as_ref().map(|root| root.join(header)) else {
            continue;
        };
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
    // **The classes a member access needs**, and the members themselves: a name whose *type* resolves is only half
    // the answer — `std::cin.read` also needs `read` to be in the class the type names.
    for class in ["std::basic_istream", "std::basic_ostream", "std::basic_string"] {
        let members = session.index().declarations_in(class, &file);
        println!(
            "   members of {class} = {} (has `read`: {} | has `size`: {})",
            members.len(),
            members.iter().any(|member| member.fact.name == "read"),
            members.iter().any(|member| member.fact.name == "size")
        );
    }
    for name in [
        "std::istream",
        "istream",
        "std::cin",
        "std::basic_istream",
        "basic_istream",
        "std::basic_ostream",
    ] {        println!(
            "   definition({name:?}) = {:?}",
            session.index().definition(name, &file).value().map(|found| (found.fact.qualified_name(), found.fact.type_of.clone()))
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
        // **And every candidate**, when the answer is not one: `Ambiguous` has several causes that read the same
        // from the answer alone, and a name two *readings* of one file both found is a different defect from a name
        // two files declare.
        let candidates = session.index().files_declaring(name, &file);
        for found in candidates.iter().take(6) {
            println!(
                "        candidate: {} name={:?} scope={:?} kind={:?} type_of={:?} local={} {:?}",
                found.file.file_name().unwrap_or_default().to_string_lossy(),
                found.fact.name,
                found.fact.scope,
                found.fact.kind,
                found.fact.type_of,
                found.fact.local,
                found.visibility
            );
        }
        // **Which reading produced them**: the raw facts of the declaring file, and what it was cooked into. Two
        // candidates that are the same declaration mean one of the two lists has it twice — or that the other one
        // has it under a different kind, which the union keeps.
        if candidates.len() > 1 {
            for found in candidates.iter().take(1) {
                let path = found.file.clone();
                let short = name.rsplit("::").next().unwrap_or(name);
                let raw = session
                    .index()
                    .summary(&path)
                    .map(|summary| {
                        summary
                            .declarations
                            .iter()
                            .filter(|fact| fact.name == short)
                            .map(|fact| format!("{:?}@{:?}", fact.kind, fact.scope))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let cooked = session
                    .index()
                    .cooked_declarations(&path)
                    .map(|facts| {
                        facts
                            .iter()
                            .filter(|fact| fact.name == short)
                            .map(|fact| format!("{:?}@{:?}", fact.kind, fact.scope))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                println!("        {short}: raw {raw:?} | cooked {cooked:?}");
            }
        }
    }
    let string_header = standard_library
        .as_ref()
        .map(|root| root.join("xstring"))
        .unwrap_or_default();
    if let Some(summary) = session.index().summary(&string_header) {
        println!("--- what <string>'s {} declarations are called ---", summary.declarations.len());
        for fact in summary.declarations.iter().take(400) {
            if matches!(fact.name.as_str(), "string" | "basic_string" | "allocator" | "char_traits" | "size_t") {
                println!("   name={:<16} qualified={:<28} scope={:?} kind={:?}", fact.name, fact.qualified_name(), fact.scope, fact.kind);
            }
        }
        let scoped = summary.declarations.iter().filter(|fact| fact.scope.is_some()).count();
        println!("   of {} declarations, {scoped} carry a scope", summary.declarations.len());
    }

    // **A header whose class never arrived.** `<istream>` is indexed and its `basic_istream` is asked for by name
    // everywhere, so what its summary actually holds is the difference between "the facts are filed wrong" and
    // "the file's body was never read": the names, in source order, with the scope each was filed under.
    let istream_header = standard_library
        .as_ref()
        .map(|root| root.join("istream"))
        .unwrap_or_default();
    if let Some(summary) = session.index().summary(&istream_header) {
        let text = std::fs::read_to_string(&istream_header).unwrap_or_default();
        let line_of = |offset: usize| text[..offset.min(text.len())].matches('\n').count() + 1;
        let scoped = summary.declarations.iter().filter(|fact| fact.scope.is_some()).count();
        println!(
            "\n--- <istream>: {} declarations, {scoped} of them scoped ---",
            summary.declarations.len()
        );
        for fact in summary.declarations.iter().take(30) {
            println!(
                "   {:>5}  {:<24} {:<24} {:?}",
                line_of(fact.range.start_offset),
                fact.name,
                fact.scope.as_deref().unwrap_or("<file scope>"),
                fact.kind
            );
        }
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
    // **What the `Ambiguous` answers would be as a list**: how many declarations each one has, and how many of
    // them are unconditionally visible. This is the measurement a plural answer is designed from — a name with two
    // declarations and a name with thirty are different problems for a client that shows them all.
    let mut candidate_sizes: Vec<(String, usize, usize)> = Vec::new();
    // Identifier → the type the analysis gives it, deduplicated: this is the data a hover shows.
    let mut typed: Vec<String> = Vec::new();
    let mut type_outcomes: HashMap<String, usize> = HashMap::new();
    // The plural query's own tally: one declaration, a list of N, or nothing — per identifier.
    let mut list_outcomes: HashMap<String, usize> = HashMap::new();
    let mut lists: Vec<(String, usize, usize, String)> = Vec::new();

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
                if let UnknownReason::Ambiguous(name) = &reason {
                    let found = session.index().files_declaring(name, &file);
                    let certain = found
                        .iter()
                        .filter(|found| {
                            found.visibility == cpp_code_analysis::IncludeVisibility::Unconditional
                        })
                        .count();
                    candidate_sizes.push((name.to_string(), found.len(), certain));
                }
                *outcomes.entry(format!("unknown: {reason:?}")).or_default() += 1;
            }
            Known::No => {
                *outcomes.entry("no name at this offset".to_string()).or_default() += 1;
            }
        }

        // **The same cursor through the plural query**, which is what a client that can show a list asks. This is
        // the measurement the list answer is judged by: a list of two is a redeclaration, seventeen is an overload
        // set, and the question is whether they are usable.
        match session.definitions(&view, offset) {
            Known::Yes(found) if found.is_one() => {
                *list_outcomes.entry("one declaration".to_string()).or_default() += 1;
            }
            Known::Yes(found) => {
                *list_outcomes
                    .entry(format!("a list of {}", found.found.len()))
                    .or_default() += 1;
                lists.push((
                    name.to_string(),
                    found.found.len(),
                    found.conditional,
                    found
                        .found
                        .first()
                        .map(|found| found.fact.qualified_name())
                        .unwrap_or_default(),
                ));
            }
            Known::Unknown(reason) => {
                *list_outcomes
                    .entry(format!("unknown: {reason:?}"))
                    .or_default() += 1;
            }
            Known::No => {
                *list_outcomes.entry("no name here".to_string()).or_default() += 1;
            }
        }

        // **The type of the name**, which is what a hover on a variable needs — and the half that was missing
        // while the standard library was unreachable: `std::string line;` has a type only if `std::string` is in
        // the index to be read.
        match session.type_at(&view, offset) {
            Known::Yes(found) => {
                *type_outcomes.entry("a type".to_string()).or_default() += 1;
                let entry = format!("{name} : {}", found.type_of);
                if !typed.contains(&entry) {
                    typed.push(entry);
                }
            }
            Known::Unknown(reason) => {
                *type_outcomes.entry(format!("unknown: {reason:?}")).or_default() += 1;
            }
            Known::No => {
                *type_outcomes.entry("no type here".to_string()).or_default() += 1;
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

    // **The candidate lists behind the `Ambiguous` answers.** By name, with how many the index holds and how many
    // of those are reachable without asking about a macro — a plural answer is only useful if its lists are short
    // enough to show and certain enough to jump to.
    if !candidate_sizes.is_empty() {
        let mut by_name: HashMap<&str, (usize, usize, usize)> = HashMap::new();
        for (name, all, certain) in &candidate_sizes {
            let entry = by_name.entry(name.as_str()).or_insert((0, *all, *certain));
            entry.0 += 1;
        }
        let mut ranked: Vec<(&str, (usize, usize, usize))> = by_name.into_iter().collect();
        ranked.sort_by_key(|(_, (asked, all, _))| (std::cmp::Reverse(*asked), *all));
        println!("\n--- the candidate lists behind `Ambiguous` ({} identifiers) ---", candidate_sizes.len());
        for (name, (asked, all, certain)) in ranked.iter().take(24) {
            println!("{asked:6} × {name:<28} {all} declaration(s), {certain} unconditional");
        }
        let sizes: Vec<usize> = candidate_sizes.iter().map(|(_, all, _)| *all).collect();
        let one = sizes.iter().filter(|size| **size == 1).count();
        let few = sizes.iter().filter(|size| (2..=5).contains(*size)).count();
        let many = sizes.iter().filter(|size| **size > 5).count();
        println!(
            "  sizes: {one} with one candidate, {few} with 2–5, {many} with more than 5 \
             (largest {})",
            sizes.iter().max().copied().unwrap_or(0)
        );

        // **How many of those candidates are one declaration recorded twice** — the measurement a "same entity"
        // rule has to be built on, and the one that decides *where* it may merge.
        //
        // The three numbers are deliberately apart:
        //   · pairs that differ in nothing a consumer can read, and stand in the **same file** — a redeclaration
        //     inside one header, which is what `cin` is (`<iostream>` writes it plain and again behind
        //     `_EXPORT_STD`). Merging these is what the code's own note asks for.
        //   · the same, in **different files** — `int count;` in two headers. The project's position, pinned by
        //     `two_declarations_of_one_name_are_a_list_for_a_consumer_that_shows_one`, is that these are **two
        //     answers**, so a rule that merged them would be wrong however identical they look.
        //   · pairs that differ only in a field the model does not fill in (an empty parameter list, a type that was
        //     never read) — indistinguishable here, and *not* the same entity in fact: two overloads of `f` whose
        //     parameters the evidence does not carry.
        let readable = |fact: &cpp_code_analysis::DeclFact, other: &cpp_code_analysis::DeclFact| {
            fact.name == other.name
                && fact.scope == other.scope
                && fact.local == other.local
                && fact.kind == other.kind
                && fact.type_of == other.type_of
                && fact.returns == other.returns
                && fact.bases == other.bases
                && fact.parameters == other.parameters
        };

        let (mut same_file, mut across_files, mut model_is_silent) = (0usize, 0usize, 0usize);
        let mut samples: Vec<String> = Vec::new();
        for (name, _, _) in &candidate_sizes {
            let Known::Yes(found) = session.index().definitions(name, &file) else {
                continue;
            };
            for (index, one) in found.found.iter().enumerate() {
                for other in found.found.iter().skip(index + 1) {
                    if !readable(&one.fact, &other.fact) {
                        continue;
                    }
                    // **Does the model say anything that could tell two declarations apart?** A function's
                    // parameter list, a variable's type, a function's return type, a class's bases. Without one of
                    // those, two facts that agree agree *because nothing was recorded* — and `std::getline`'s four
                    // overloads in `<string>` are exactly that: four functions whose parameters this model does not
                    // carry. Merging those would be inventing an identity out of silence.
                    let says_something = !one.fact.parameters.is_empty()
                        || one.fact.type_of.is_some()
                        || one.fact.returns.is_some()
                        || !one.fact.bases.is_empty();

                    if !says_something {
                        model_is_silent += 1;
                    } else if one.file == other.file {
                        same_file += 1;
                        if samples.len() < 8 {
                            samples.push(format!(
                                "{name} in {}",
                                one.file.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                    } else {
                        across_files += 1;
                    }
                }
            }
        }
        println!(
            "  identical candidates: {same_file} pair(s) in **one file** with something recorded to tell them \
             apart, {across_files} across files (two answers), {model_is_silent} that agree only because nothing \
             is recorded (parameters, type, returns and bases all empty)"
        );
        if !samples.is_empty() {
            println!("     in one file: {}", samples.join(" | "));
        }
    }

    println!("\n--- the type of every identifier, deduplicated (what a hover has to show) ---");    let mut ranked: Vec<(String, usize)> = type_outcomes.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (outcome, count) in &ranked {
        println!("{count:6}  {outcome}");
    }
    for entry in typed.iter().take(24) {
        println!("        {entry}");
    }

    // **The member list of a type spelled as an alias**, which is the question `std::cin.eof()` asks: the object's
    // type is written `istream` (inside `std`), and `istream` is `basic_istream<char, char_traits<char>>`.
    for written in ["istream", "std::istream", "std::basic_istream"] {
        let members = session.members_of(&view, written).value();
        println!(
            "   members_of({written:?}) = {:?}, has `eof`: {}, unlisted bases: {:?}",
            members.as_ref().map(|list| list.members.len()),
            members
                .as_ref()
                .is_some_and(|list| list.members.iter().any(|member| member.fact.name == "eof")),
            members
                .as_ref()
                .map(|list| {
                    list.unlisted
                        .iter()
                        .map(|base| base.spelling.clone())
                        .collect::<Vec<_>>()
                })
        );
    }

    // **What the completion list is made of**: how many files the cursor's file can see, and how many of them are
    // reachable only through an `#include` inside an `#if` — which is where a *plausible* list turns into a payload,
    // measured at the shell: 14351 items, 4.5 MB of JSON, for a blank line in a 78-line file.
    let visible = session.index().visible_files(&file);
    let unconditional = visible
        .iter()
        .filter(|(_, visibility)| *visibility == cpp_code_analysis::IncludeVisibility::Unconditional)
        .count();
    println!(
        "\n--- what the cursor's file can see: {} files, {unconditional} unconditionally ---",
        visible.len()
    );
    for wanted in ["arm_neon.h", "arm64_neon.h", "zmmintrin.h", "immintrin.h"] {
        for (path, visibility) in &visible {
            if std::path::Path::new(path)
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == wanted)
            {
                println!("   {wanted:<16} {visibility:?}");
            }
        }
    }

    // **What the plural query answers**, over the same identifiers: this is the number that says whether
    // `textDocument/definition` can answer where the single-answer form said `Ambiguous`.
    println!("\n--- every identifier, through the plural query ---");
    let mut ranked: Vec<(String, usize)> = list_outcomes.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (outcome, count) in &ranked {
        println!("{count:6}  {outcome}");
    }
    let mut by_name: HashMap<&str, (usize, usize, usize)> = HashMap::new();
    for (name, all, conditional, _) in &lists {
        let entry = by_name.entry(name.as_str()).or_insert((0, *all, *conditional));
        entry.0 += 1;
    }
    let mut ranked: Vec<(&str, (usize, usize, usize))> = by_name.into_iter().collect();
    ranked.sort_by_key(|(_, (asked, all, _))| (std::cmp::Reverse(*asked), *all));
    for (name, (asked, all, conditional)) in ranked.iter().take(16) {
        println!("{asked:6} × {name:<24} a list of {all} ({conditional} conditional)");
    }

    // **The names a `::` offers** — the user's first symptom, asked generically: every qualification the file
    // writes is a scope, and the question at it is the one a completion at the cursor asks.
    println!("\n--- completion after every `::` in the file ---");
    let mut at = 0usize;
    while let Some(found) = source[at..].find("::") {
        let after = at + found + 2;
        at = after;
        let cursor = after + source[after..].bytes().take_while(|byte| *byte == b' ').count();
        let line = line_of(cursor) + 1;
        let text = source.lines().nth(line - 1).unwrap_or_default().trim_end().to_string();
        match session.name_completions(&view, cursor) {
            Known::Yes(completions) => println!(
                "{:>4}:{:<4} scope {:<28} {} names, e.g. {:?}",
                line,
                cursor - source[..cursor].rfind('\n').map_or(0, |at| at + 1),
                format!("{:?}", completions.scope),
                completions.names.len(),
                completions
                    .names
                    .iter()
                    .take(6)
                    .map(|name| name.fact.name.as_str())
                    .collect::<Vec<_>>()
            ),
            Known::Unknown(reason) => println!(
                "{:>4}  scope NOT ANSWERED: {}  | {}",
                line,
                reason.describe(),
                &text[..text.len().min(60)]
            ),
            Known::No => println!("{line:>4}  no name position"),
        }
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

    // ---------------------------------------------------------------------------------------------
    // **What a unit read adds, and what it displaces** — the audit for the one capability this session
    // built and did not wire (`Session::read_the_unit`: it changes three readings the wrong way, and the
    // reason is not understood).
    //
    // Both sides are measured **in this one process**, because the question is a difference and two runs
    // are two machines: the index's answer for `std` before and after, split by file and by reading —
    // "the total went down by 49" is a symptom, and the file it went down *in* is the cause.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- a unit read, before and after ---");

    fn std_by_file(
        session: &cpp_code_analysis::Session,
    ) -> Vec<(String, usize, usize, bool)> {
        let mut rows = Vec::new();
        for summary in session.index().summaries() {
            let raw = summary
                .declarations
                .iter()
                .filter(|fact| fact.scope.as_deref() == Some("std"))
                .count();
            let cooked = session.index().cooked_declarations(&summary.path);
            let has = cooked.is_some();
            let cooked = cooked
                .map(|facts| {
                    facts
                        .iter()
                        .filter(|fact| fact.scope.as_deref() == Some("std"))
                        .count()
                })
                .unwrap_or(0);
            if raw + cooked > 0 {
                rows.push((summary.path.display().to_string(), raw, cooked, has));
            }
        }
        rows.sort();
        rows
    }

    let std_total =
        |session: &cpp_code_analysis::Session| session.index().declarations_in("std", &file).len();

    let before_total = std_total(&session);
    let before = std_by_file(&session);
    let before_cooked: usize = before.iter().map(|(_, _, cooked, _)| cooked).sum();
    let before_cooked_files = before.iter().filter(|(_, _, _, has)| *has).count();

    let reading = session.read_the_unit(&file);
    println!("unit read: {reading:?}");

    let after_total = std_total(&session);
    let after = std_by_file(&session);
    let after_cooked: usize = after.iter().map(|(_, _, cooked, _)| cooked).sum();
    let after_cooked_files = after.iter().filter(|(_, _, _, has)| *has).count();

    println!(
        "declarations_in(\"std\") {before_total} → {after_total} | declarations in `std`: \
         raw+cooked {before_cooked} → {after_cooked}, over {before_cooked_files} → \
         {after_cooked_files} files with a cooked reading"
    );

    let by_path: std::collections::HashMap<&str, (usize, usize)> = before
        .iter()
        .map(|(path, raw, cooked, _)| (path.as_str(), (*raw, *cooked)))
        .collect();
    let mut moved = 0usize;
    for (path, raw, cooked, _) in &after {
        let (raw_before, cooked_before) = by_path.get(path.as_str()).copied().unwrap_or((*raw, 0));
        if raw_before != *raw || cooked_before != *cooked {
            moved += 1;
            if moved <= 20 {
                println!(
                    "   {:<22} raw {raw_before} → {raw} | cooked {cooked_before} → {cooked}",
                    path.rsplit(['\\', '/']).next().unwrap_or(path)
                );
            }
        }
    }
    println!("   {moved} files changed either reading's `std` count");

    // ---------------------------------------------------------------------------------------------
    // **What one keystroke in a function body costs** — the measurement the "boundary rule" has to be
    // built on (Claude's review §6 item 3, from `plan-units.md` §5 阶段 3).
    //
    // `Session::buffer_changed` drops **every translation unit** on any edit, so the next cook re-walks the
    // whole closure — 138 files here — to answer a question a body edit cannot have changed. The instrument
    // is the one already in the crate: `Stage::Walk` is the walk, `Stage::Closure` its closure read, and the
    // session's own `pending_work` says how much work the edit queued.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- ten edits in a function body ---");
    {
        let body = session
            .text(&file)
            .expect("the file the probe was pointed at");
        session.did_open(&file, &body);
        session.index_everything();

        let before = std::time::Instant::now();
        let mut edited = body.clone();

        // **Two passes, and only the second one is measured.** The first run of this held the disk cache cold, and
        // that difference alone was a factor of two between two runs of the same code (`1.271 s` against
        // `0.556 s`) — a number that changes with the state of a cache is not a measurement of the code. The warm-up
        // is the same ten edits, thrown away.
        let edit = |edited: &mut String, session: &mut cpp_code_analysis::Session| {
            if let Some(at) = edited.find("shutdownRequested = false;") {
                edited.insert(at, ' ');
            }
            session.did_change(&file, edited);
            session.index_everything();
        };
        for _ in 0..10 {
            edit(&mut edited, &mut session);
        }

        cpp_code_analysis::stages::StageTimes::reset();
        let measured = std::time::Instant::now();
        for _ in 0..10 {
            edit(&mut edited, &mut session);
        }
        let wall = measured.elapsed();
        let _ = before;

        println!("   10 body edits, warm: {wall:?} wall — every stage, in this window only:");
        print!("{}", cpp_code_analysis::stages::StageTimes::read().report());
    }
    // **What the unit's cooked reading actually says** — the scopes, not the count. A cooked `std` count of zero
    // beside 57 files that have a reading means the scope walk over the unit's rendering never opened `std`; the
    // three lines below say whether that is "no scopes at all" or "scopes that are not `std`", which are two
    // different defects (a rendering that lost `_STD_BEGIN`'s expansion, or a walk that cannot see it).
    {
        let cstdio = session
            .index()
            .summaries()
            .find(|summary| summary.path.ends_with("cstdio"))
            .map(|summary| summary.path.clone());
        if let Some(cstdio) = cstdio
            && let Some(facts) = session.index().cooked_declarations(&cstdio)
        {
            let scoped = facts.iter().filter(|fact| fact.scope.is_some()).count();
            println!(
                "   cstdio's unit reading: {} declarations, {scoped} with a scope — first ones:",
                facts.len()
            );
            for fact in facts.iter().take(8) {
                println!("      {:<18} scope={:?} kind={:?}", fact.name, fact.scope, fact.kind);
            }
        }

        // **Where the wrong scope came from**, asked of the index rather than guessed: the namespace fact itself
        // (which file, which offset) and one fact that ended up inside it. A namespace that encloses a *later*
        // file's `std` is either an unbalanced file or a close that the walk never paired, and the two have
        // different offsets — this is the line that tells them apart.
        for summary in session.index().summaries() {
            let Some(facts) = session.index().cooked_declarations(&summary.path) else {
                continue;
            };
            for fact in facts {
                if fact.name == "vc_attributes" || fact.scope.as_deref() == Some("vc_attributes") {
                    println!(
                        "   {} @{}..{} | name={} scope={:?} kind={:?}",
                        summary
                            .path
                            .to_string_lossy()
                            .rsplit(['\\', '/'])
                            .next()
                            .unwrap_or_default(),
                        fact.range.start_offset,
                        fact.range.end_offset(),
                        fact.name,
                        fact.scope,
                        fact.kind
                    );
                }
            }
        }
    }
}
