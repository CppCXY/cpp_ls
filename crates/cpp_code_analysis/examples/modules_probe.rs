//! **What the analysis makes of a real module project** — the fixture in `tests/fixtures/modules`, built and run
//! by `target/build_modules.bat` so that it is known to be valid C++20 rather than a guess.
//!
//! ```text
//! cargo run --release --example modules_probe -- <dir-with-modules> [<file.cpp>]
//! ```
//!
//! It answers three questions, in the order that separates "the grammar is read" from "the answers are right":
//!
//! 1. **What does each file declare and import?** — the parse, per file, through [`ModuleInfo`]. A file the project
//!    scan never picked up (`.ixx`, `.cppm`) shows up here as missing rather than as empty.
//! 2. **What does the import graph resolve to?** — `scan_imports`, with each edge's outcome. This is the layer
//!    that already existed and is the one that knows *where* a module lives.
//! 3. **Can a reader name what the import brought in?** — the product question: `mathlib::add` from a file that
//!    says `import mathlib;`. This is where the layer below and the layer above meet, and the point of the probe is
//!    to say plainly which of the two is missing when the answer is nothing.

use cpp_code_analysis::{
    DiskFiles, ModuleInfo, OpenDocuments, PathInterner, Session, SessionFiles, WatchFilter,
    scan_imports,
};
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    // ---------------------------------------------------------------------------------------------
    // 1. What each file declares, read from the parse — the grammar half.
    // ---------------------------------------------------------------------------------------------
    println!("--- what each file declares ---");
    let mut trees: Vec<(PathBuf, String)> = Vec::new();

    for entry in std::fs::read_dir(&root).expect("the fixture directory") {
        let path = entry.expect("a directory entry").path();
        if !path.is_file() {
            continue;
        }
        let Some(source) = std::fs::read_to_string(&path).ok() else {
            continue;
        };

        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let info = ModuleInfo::from_tree(&tree.get_red_root());

        let unit = match info.unit {
            Some(unit) => unit.describe(),
            None => "no module declaration",
        };

        println!(
            "   {:<16} {unit:<24} module {:?} partition {:?}{}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            info.module_name.as_deref().unwrap_or("-"),
            info.partition_name.as_deref(),
            if info.has_global_fragment { " (global fragment)" } else { "" },
        );

        for import in &info.imports {
            println!(
                "       import {:<16} {}",
                import.target.describe(),
                if import.is_reexport { "re-exported" } else { "" }
            );
        }

        trees.push((path, source));
    }

    // ---------------------------------------------------------------------------------------------
    // 2. The import graph — the resolution half, which is a library that already exists.
    //
    // **The session's own configuration**, not a default one: `import <iostream>;` is resolved through the include
    // search and `import std;` through the toolchain's `modules` directory, so a scan with no include paths answers
    // "cannot find it" for both and measures nothing.
    // ---------------------------------------------------------------------------------------------
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    println!("\n--- the import graph from {} ---", file.display());
    let Some((_, source)) = trees.iter().find(|(path, _)| path == &file) else {
        println!("   {} is not in the directory", file.display());
        return;
    };
    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    let mut interner = PathInterner::new(cfg!(windows));
    let graph = scan_imports(&tree, &file, &DiskFiles, session.config(), &mut interner);

    for unit in &graph.units {
        println!(
            "   unit {:<20} in {}",
            unit.qualified_name(),
            interner
                .path(unit.file)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "?".to_string())
        );
    }
    for edge in &graph.edges {
        println!(
            "   {} --{}{}--> {}",
            interner
                .path(edge.from)
                .map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .unwrap_or_else(|| "?".to_string()),
            if edge.is_reexport { "export " } else { "" },
            edge.target.describe(),
            edge.outcome.describe(),
        );
    }

    // ---------------------------------------------------------------------------------------------
    // 3. The product question: does `import mathlib;` make `mathlib::add` nameable?
    // ---------------------------------------------------------------------------------------------
    println!("\n--- can a reader who wrote `import mathlib;` name what it exports? ---");
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    println!("   project files the scan found: {}", session.project_files().len());
    for path in session.project_files() {
        println!("      {}", path.display());
    }

    let Some(view) = session.view(&file) else {
        println!("   {} is not held", file.display());
        return;
    };

    // **What the asking file says it imports, before any name is looked up.** The two imports a `std::` answer can
    // come from are told apart here: a header unit is followed as an `#include`, so a file that has both may answer
    // from the header while the *named* import answers nothing.
    if let Some(reading) = session.index().summary(&file) {
        println!(
            "   this file's own reading: imports {:?}, header units {:?}",
            reading.modules.imports,
            reading.modules.header_units,
        );
    }

    // The names asked about are the caller's third argument, space-separated, when one is given: the fixture has
    // files that import different things, and the same question has a different answer in each.
    let asked: Vec<String> = std::env::args()
        .nth(3)
        .map(|list| list.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| {
            ["mathlib::add", "mathlib::multiply", "Point", "manhattan", "area_of"]
                .iter()
                .map(|name| name.to_string())
                .collect()
        });

    // **What a reader is told when a module cannot be read at all.** Asked *before* the read below, because the
    // read is what changes the answer: a note is what the reader sees on a machine whose build has not named the
    // module yet, and it is not an error — it says where this compiler keeps modules and what would let it find
    // this one. Empty for the ordinary project, whose modules are its own files.
    let notes = session.notes_about_the_modules(&view);
    if notes.is_empty() {
        println!("   nothing to be told about this file's imports: every module it names is in the project");
    } else {
        for note in &notes {
            println!("   note at byte {}: {}", note.start, note.message);
        }
    }

    for name in &asked {
        // **The plural question**, because the singular one is not what the LSP asks: `definition` collapses several
        // declarations into `Known::Unknown(Ambiguous)`, and `.value()` turns that into `None` — which reads as
        // "nothing declares it". Measured that way, `std::vector` looked like a hole in the index; asked properly it
        // is **four** declarations, and the client is given four locations (see the definitions printed below).
        println!(
            "   definition({name:<18}) = {}",
            describe(&session, name, &file),
        );

        // **The second reading of the same question**, after the module interface units this file imports have been
        // read in. Before that, a module whose interface unit is outside the project (`import std;`) has no summary
        // anywhere, so the answer is "not declared" — which is the state the read exists to change, and the reason
        // the two columns are printed rather than one. The clock is around it because this runs inside a request.
        let started = std::time::Instant::now();
        let read_in = session.read_the_modules_a_file_imports(&file);
        let took = started.elapsed();

        if read_in > 0 {
            println!(
                "      …after reading {read_in} module interface unit(s) in, {:.1} ms: {}",
                took.as_secs_f64() * 1000.0,
                describe(&session, name, &file),
            );
        } else {
            println!(
                "      …and asking again read nothing in: {}",
                describe(&session, name, &file),
            );
        }
    }

    // **What the *second* request pays**, said out loud rather than implied: the whole design rests on this being
    // nothing, and a number that is only "the loop printed nothing the second time" is not a measurement.
    let started = std::time::Instant::now();
    let asked_again = session.read_the_modules_a_file_imports(&file);
    println!(
        "   asking again after everything is in the index: {asked_again} file(s) read in {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0,
    );

    // **Why a name that is in the library answers nothing** — the candidates the index actually holds, which is the
    // only way to tell "not declared" from "declared, and the answer came out plural" from "declared under a name I
    // did not think of". `definition` collapses those three into `None`/`Unknown`, and this prints the list instead.
    for name in ["std::string", "std::vector", "std::basic_string", "std::basic_string_view", "std::char_traits"] {
        match session.index().definitions(name, &file) {
            cpp_code_analysis::Known::Yes(definitions) => {
                println!("   {name:<26} → {} declaration(s)", definitions.found.len());
                for declaration in definitions.found.iter().take(3) {
                    println!(
                        "      {:?} of {:?} in {}",
                        declaration.fact.kind,
                        declaration.fact.scope,
                        declaration.file.display(),
                    );
                }
            }
            other => println!("   {name:<26} → {other:?}"),
        }
    }

    // The interface units the file's imports named: printed so that a `None` above can be attributed to "nothing
    // declares it" rather than to "nothing looked for it".
    if let Some(reading) = session.index().summary(&file) {
        for imported in &reading.modules.imports {
            println!(
                "   module {imported:<12} is declared by {:?}",
                session.index().interface_unit_of(imported)
            );
        }
    }

    // …and the completion a reader gets after a qualified name in this file, which is the answer they see. Both the
    // scope and the position come from the file itself: a fixture that says `std::string` is not asked about
    // `mathlib::`.
    // …and the completion a reader gets after a qualified name in this file, which is the answer they see. Both the
    // scope and the position come from the file itself: a fixture that says `std::string` is not asked about
    // `mathlib::`.
    let text = view.source.to_string();
    for (written, scope) in [("mathlib::add", "mathlib::"), ("std::cou", "std::")] {
        let Some(at) = text.find(written).map(|at| at + scope.len()) else {
            continue;
        };

        let found = session.completions(&view, at);
        println!(
            "   completion after `{scope}` → {} item(s){}{}",
            found.items.len(),
            if found.scope.is_empty() { String::new() } else { format!(", scope {:?}", found.scope) },
            if found.truncated { ", truncated" } else { "" },
        );
        for item in found.items.iter().take(8) {
            println!("      {}", item.label);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // 4. **What `import std;` costs when nothing is cached.** Section 3's clock is the warm number: the summaries
    //    survive on disk between runs, so the second time a request asks, the four hundred files are decoded rather
    //    than parsed. The first time on a machine is the one that decides whether this is a feature or a stall, and
    //    the way to see it is to **delete the summaries this session writes** and ask again — a second session over a
    //    scratch root would be a session with none of the *toolchain* the first one discovered, and it would measure
    //    a standard library read without the compiler's own macros in force. That mistake was made here once: the
    //    scratch session read 393 files and then answered `None` for `std::string`, which is a cold number for the
    //    wrong program.
    // ---------------------------------------------------------------------------------------------
    let std_interface = std::env::var_os("CPPLS_STD_IXC").map(PathBuf::from).or_else(|| {
        // Found the way the session found it: the toolchain's `modules` directory beside the include paths.
        session.config().include_paths.iter().find_map(|directory| {
            let candidate = directory.directory.parent()?.join("modules").join("std.ixx");
            candidate.is_file().then_some(candidate)
        })
    });

    match std_interface.as_deref() {
        None => println!("\n--- what `import std;` costs ---\n   no `modules/std.ixx` found; skipped"),
        Some(std_interface) => {
            println!("\n--- what `import std;` costs to read ({}) ---", std_interface.display());
            let bytes = std::fs::read_to_string(std_interface).map(|text| text.len()).unwrap_or(0);
            println!("   the interface unit is {bytes} bytes");

            let cache = session.cache_directory().to_path_buf();
            let _ = std::fs::remove_dir_all(&cache);
            println!("   the summaries under {} deleted, so the read below is the first one", cache.display());

            // **The same root and the same discovery, one more time**: the compiler is asked again exactly as the
            // first session asked it, so the macros in force while reading the standard library are the same. What is
            // missing is only what was on disk.
            let mut cold_read_session = Session::open(
                root.clone(),
                SessionFiles::new(OpenDocuments::new(), DiskFiles),
                WatchFilter::new(&root),
            );
            cold_read_session.add_project_files([std_interface.to_path_buf()]);

            let started = std::time::Instant::now();
            let read = cold_read_session.index_everything();
            let cold_read = started.elapsed();

            // **One kind of cooked reading, not every file's.** `want_everything_cooked` on the whole standard
            // library is a batch job nobody asks for on a keystroke; what a query costs is the *one* file it needs.
            let started = std::time::Instant::now();
            cold_read_session.catch_up(std_interface);
            let catching_up = started.elapsed();

            println!(
                "   with nothing cached, {read} file(s) read in {:.0} ms, and the answers are the same ones",
                cold_read.as_secs_f64() * 1000.0,
            );
            println!(
                "   the interface unit's own cooked reading: {:.1} ms; its summary declares {} name(s) of its own, \
                 which is why the names come from the headers it includes",
                catching_up.as_secs_f64() * 1000.0,
                cold_read_session
                    .index()
                    .summary(std_interface)
                    .map(|summary| summary.declarations.len())
                    .unwrap_or(0),
            );

            for name in ["std::string", "std::vector", "std::cout"] {
                println!(
                    "   definition({name:<14}) = {}",
                    describe(&cold_read_session, name, std_interface),
                );
            }
        }
    }

    println!();
}

/// **What the index says a name is**, as the walk that was asked would answer it.
///
/// The plural query, and the count when there is more than one: `definition` answers `Ambiguous` for a name that is
/// declared twice, and a probe that printed `Option::None` for that would report a name the index holds perfectly
/// well as a hole in it — which is exactly the mistake this function exists to stop making.
fn describe<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    name: &str,
    from: &Path,
) -> String {
    match session.index().definitions(name, from) {
        cpp_code_analysis::Known::Yes(found) if found.found.len() == 1 => found.found[0]
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| found.found[0].file.display().to_string()),
        cpp_code_analysis::Known::Yes(found) => format!(
            "{} declarations in {}",
            found.found.len(),
            found
                .found
                .iter()
                .map(|declaration| declaration
                    .file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => format!("{other:?}"),
    }
}

