//! **A C++23 module project, read the way a request reads it** — what the four editor questions answer, and what
//! each one costs.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example module_request_probe -- <project root> <a file in it>
//! ```
//!
//! Reported: *"it barely understands modules; in a file with modules it can hardly complete anything, and a hover
//! takes very long"*. Those are three different claims and this probe separates them, because "no completion" has
//! three unrelated causes and only one of them is about modules:
//!
//! ```text
//! 1  the parse does not see the module declaration      ModuleInfo, per file
//! 2  the imports are not read into the index            Session::read_the_modules_a_file_imports, and then
//!                                                       ProjectIndex::definitions("math::add", …)
//! 3  the query is slow                                  the request's four steps, timed apart
//! ```
//!
//! The request path is the server's, in the server's order: `catch_up` (this file, this include closure) then
//! `read_the_modules_a_file_imports` (the modules it names) then the query. A module is **not** in the include
//! closure, which is the whole reason the second step exists and the first place to look when a module file
//! answers nothing.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("a project root"));
    let file = PathBuf::from(args.next().expect("a file in it"));
    let text = std::fs::read_to_string(&file).expect("the file is readable");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.did_open(&file, &text);

    // ---------------------------------------------------------------------------------------------
    // 1. The request, in the server's order, with each step timed.
    // ---------------------------------------------------------------------------------------------
    // **What the project discovery found**, before any compiler is asked: a CMake project keeps its
    // `compile_commands.json` in the build directory, and a session that misses it configures the analysis with
    // whatever the machine's default toolchain is — a different standard library than the one the project builds.
    {
        let discovery = cpp_code_analysis::project::discover(&DiskFiles, &root, None);
        println!(
            "   discovery: database {:?}, {} problem(s)",
            discovery
                .database
                .as_ref()
                .map(|database| database.path.display().to_string()),
            discovery.problems.len()
        );
        for problem in &discovery.problems {
            println!("      {:?}", problem);
        }
        println!("      flags from it: {:?}", discovery.flags());
    }

    // **What the session was configured with** — the includes in a global module fragment are found by this search,
    // and a session with no search paths renders every `#include` as nothing at all.
    println!(
        "   config: {} user include path(s), {} system path(s), dialect {:?}",
        session.config().user_include_paths().count(),
        session.config().system_include_paths().count(),
        session.config().dialect(),
    );
    for path in session
        .config()
        .user_include_paths()
        .chain(session.config().system_include_paths())
        .take(8)
    {
        println!("      {}", path.display());
    }

    let step = Instant::now();
    session.catch_up(&file);
    let caught_up = step.elapsed();

    // **What the file's own reading says it imports**, before any module is read in. A file whose summary lists no
    // import cannot be helped by the step below, so this is the first thing to print.
    match session.index().summary(&file) {
        Some(summary) => println!(
            "   the reading: module {:?} partition {:?} interface {} | imports {:?} | header units {:?}",
            summary.modules.module,
            summary.modules.partition,
            summary.modules.is_interface,
            summary.modules.imports,
            summary.modules.header_units,
        ),
        None => println!("   the file has no summary at all"),
    }
    println!(
        "   imports_a_module_that_is_not_read: {}",
        session.imports_a_module_that_is_not_read(&file)
    );

    let step = Instant::now();
    let modules_read = session.read_the_modules_a_file_imports(&file);
    let modules = step.elapsed();

    // **Why that number is what it is.** The request path resolves a module name through
    // `ModuleScanner::resolve_module`, whose second step is the naming convention. If that step fails, the answer
    // is "0 modules read" and the caller never learns why — so the same question is asked here, with the outcome.
    {
        use cpp_code_analysis::{PathInterner, sema::modules::ModuleScanner};

        let mut interner = PathInterner::new(cfg!(windows));
        let mut scanner = ModuleScanner::new(&DiskFiles, session.config());
        for module in ["math", "shapes"] {
            let outcome = scanner.resolve_module(module, &file, &mut interner);
            println!(
                "   scanner.resolve_module({module:?}): {}",
                outcome.describe()
            );
        }
        println!(
            "   the scanner's units: {:?}",
            scanner
                .units()
                .iter()
                .map(|unit| unit.path.display().to_string())
                .collect::<Vec<_>>()
        );
    }

    println!(
        "   after it: interface_unit_of(\"math\") = {:?}, imports_a_module_that_is_not_read: {}",
        session.index().interface_unit_of("math"),
        session.imports_a_module_that_is_not_read(&file)
    );

    // **The cook**, which is the rest of what the server's `prepare` does: without a rendering no query answers, and
    // this is the half a reader waits for on the first request in a file.
    let step = Instant::now();
    session.index_everything();
    let indexed = step.elapsed();

    let step = Instant::now();
    let view = session.view(&file);
    let viewed = step.elapsed();

    println!(
        "a cold request: catch_up {} ms, read_the_modules {} module(s) {} ms, index_everything {} ms, view {} ms \
         ({} file(s) in the index)",
        caught_up.as_millis(),
        modules_read,
        modules.as_millis(),
        indexed.as_millis(),
        viewed.as_millis(),
        session.index().len()
    );
    let Some(view) = view else {
        println!("   the file has no cooked reading, so no query below can answer");
        return;
    };

    // **What a scope-level query says about the three namespaces this project is about.** `std` comes from the
    // system headers, `math` and `shapes` from the module interface units — and a completion after `X::` is a
    // question about exactly this list.
    println!(
        "   visible_files: {} file(s) are in view of this one | declarations_in(\"std\") = {} | \
         declarations_in_where(\"std\") = {} | scope_postings = {}",
        session.index().visible_files(&view.path).len(),
        session.index().declarations_in("std", &view.path).len(),
        session
            .index()
            .declarations_in_where("std", &view.path, |_| true)
            .len(),
        session.index().scope_postings("std"),
    );
    for scope in ["std", "math", "shapes"] {
        let listed = session.members_of(&view, scope);
        println!(
            "   members_of({scope:?}): {}",
            match &listed {
                Known::Yes(list) => format!(
                    "{} member(s), unlisted {}",
                    list.members.len(),
                    list.unlisted.len()
                ),
                other => format!("{other:?}"),
            }
        );
    }

    // **What the compiler's reading of this file is.** A global module fragment is `module;` … `module m;`, and the
    // includes in it are what the file's own names come from — so a rendering that does not contain them is a file
    // whose `std::` completion is empty for a reason that has nothing to do with modules.
    println!(
        "   rendering: {} bytes, {} line(s) | `namespace std` {} | `sqrt` {} | `#include` {}",
        view.source.len(),
        view.source.lines().count(),
        view.source.contains("namespace std"),
        view.source.contains("sqrt"),
        view.source.contains("#include"),
    );
    let head: String = view.source.chars().take(200).collect();
    println!("   first 200: {:?}", head.replace('\n', "⏎"));

    // ---------------------------------------------------------------------------------------------
    // 2. What the index holds for the names `import math;` brings in.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- what the index holds, after the request read the modules ---");
    // **What the interface unit's summary says about the module's own names**, which is the question a missing
    // completion turns on: is `math` a fact at all, and is it marked exported for the files that import it?
    for unit in session.project_files() {
        if !unit.to_string_lossy().ends_with("math.cppm") {
            continue;
        }
        if let Some(summary) = session.index().summary(unit) {
            println!(
                "   summary of {}: {} facts",
                unit.display(),
                summary.declarations.len()
            );
            for fact in &summary.declarations {
                if fact.name == "math" || fact.scope.as_deref() == Some("math") {
                    println!(
                        "      {:?} kind {:?} scope {:?} exported {} @ {}",
                        fact.name, fact.kind, fact.scope, fact.exported, fact.range.start_offset
                    );
                }
            }
        }
    }
    println!(
        "   facts in scope `std`: {} | postings named `vector`: {} | facts named `add`: {}",
        session.index().scope_postings("std"),
        session.index().name_postings("vector"),
        session.index().name_postings("add"),
    );
    for name in [
        "std",
        "std::size_t",
        "math",
        "math::add",
        "math::Vec2",
        "shapes",
        "shapes::Circle",
        "std::vector",
    ] {
        let found = session.index().definitions(name, &file);
        let said = match &found {
            Known::Yes(list) => format!("{} declaration(s)", list.found.len()),
            other => format!("{other:?}"),
        };
        println!("   {name:<16} {said}");
    }

    // ---------------------------------------------------------------------------------------------
    // 3. The four questions, with what each costs.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- the queries ---");

    // **The questions are the caller's**: each argument is a prefix the completion is asked right after
    // (`math::`, `vec.`, `std::`, `namespace math {`). The defaults are the two that a module project raises.
    let mut asked: Vec<String> = args.collect();
    if asked.is_empty() {
        asked = ["math::", "vec."]
            .iter()
            .map(|ask| ask.to_string())
            .collect();
    }

    for needle in &asked {
        let Some(at) = text.find(needle.as_str()).map(|at| at + needle.len()) else {
            println!("   `{needle}`: not in this file");
            continue;
        };
        // **What the completion thinks the cursor is**, which is the question every branch below turns on: a
        // `Qualified` context names a scope and a prefix, and the prefix is what the list is filtered by. The tree
        // is the **rendering's**, so the file offset is translated first — passing one straight in panics inside
        // rowan (`Bad offset: range 0..366 offset 938`), which is the same two-coordinate-systems seam the analysis
        // keeps paying for.
        match view.reading_offset_of(FileOffset::new(at)) {
            Some(reading) => println!(
                "   context at {at} (reading {}): {:?}",
                reading.get(),
                cpp_code_analysis::completion::context_at(&view.root, reading.get())
            ),
            None => println!("   context at {at}: not in the rendering"),
        }
        let step = Instant::now();
        let offered = session.completions(&view, FileOffset::new(at));
        let took = step.elapsed();
        let labels: Vec<&str> = offered
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        println!(
            "   `{needle}` completion at {at}: {} item(s) in {} ms",
            labels.len(),
            took.as_millis()
        );
        println!(
            "      {}",
            labels
                .iter()
                .take(24)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // **Signature help, which the user reports as "almost never appearing".** The handler declines to answer while
    // the file is not locally ready, so both halves are printed: what the call resolves to, and whether the handler
    // would have returned `None` before asking.
    println!("\n--- signature help ---");
    println!("   is_locally_ready: {}", session.is_locally_ready(&file));
    for needle in ["math::add(", "std::println(", "std::format("] {
        let Some(at) = text.find(needle).map(|at| at + needle.len()) else {
            println!("   `{needle}`: not in this file");
            continue;
        };
        let reading = view
            .reading_offset_of(FileOffset::new(at))
            .map_or(view.source.len(), |at| at.get());
        let signatures = session.signatures_at(&view, ReadingOffset::new(reading));
        let labels: Vec<&str> = signatures.iter().map(|one| one.label.as_str()).collect();
        println!(
            "   `{needle}` at {at} (reading {reading}): {} signature(s) {:?}",
            labels.len(),
            labels
        );
        // **Where the cursor actually landed**, in both texts: a needle can match inside a comment, and the two
        // coordinate systems disagree by exactly what the cook removed.
        let window = |source: &str, at: usize| {
            source
                .get(at.saturating_sub(48)..at.min(source.len()))
                .unwrap_or_default()
                .replace('\n', " ")
        };
        println!("      file:      …{}|", window(&text, at));
        println!("      rendering: …{}|", window(&view.source, reading));
        // **Why it is zero.** The popup asks the same question the definition does, so its answer is printed next
        // to it: a `::`-qualified name this file does not declare has to reach the index, and the shape it arrives
        // in decides whether the index is consulted at all — `No` and `Unknown(NotDeclaredHere)` take two different
        // branches in `callees_of_a_call`.
        let name_at = reading.saturating_sub(needle.trim_end_matches('(').len());
        match session.definitions(&view, ReadingOffset::new(name_at)) {
            Known::Yes(list) => println!("      definitions: {} declaration(s)", list.found.len()),
            Known::Unknown(reason) => println!("      definitions: Unknown({reason:?})"),
            Known::No => println!("      definitions: No"),
        }
    }

    // The same questions with the whole project indexed — is the answer *better* once everything is read, or the
    // same? And what do hover's own queries say, in the **reading's** offsets rather than the file's?
    println!(
        "\n--- after the whole project is indexed ({} file(s)) ---",
        session.index().len()
    );
    if let Some(view) = session.view(&file) {
        for needle in &asked {
            let Some(at) = text.find(needle.as_str()).map(|at| at + needle.len()) else {
                continue;
            };
            let offered = session.completions(&view, FileOffset::new(at));
            let labels: Vec<&str> = offered
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect();
            println!(
                "   `{needle}` completion: {} item(s) | {}",
                labels.len(),
                labels
                    .iter()
                    .take(16)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // hover's own queries: the type of the object before a `.`, and the declaration of the name after a `::`.
        for needle in asked
            .iter()
            .filter(|needle| needle.ends_with('.') || needle.ends_with("::"))
        {
            let Some(at) = text.find(needle.as_str()).map(|at| at + needle.len()) else {
                continue;
            };
            // The rendering's offset for the same place — a hover arrives as a file position, and the tree it is
            // asked about is the rendering's.
            let Some(reading) = view.reading_offset_of(FileOffset::new(at)) else {
                println!("   `{needle}`: not in the rendering");
                continue;
            };
            let step = Instant::now();
            let found = session.definition(&view, reading);
            let took = step.elapsed();
            let said = match &found {
                Known::Yes(definition) => format!(
                    "{}:{}",
                    definition
                        .file
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    definition.fact.name
                ),
                other => format!("{other:?}"),
            };
            println!(
                "   definition at `{needle}`: {said} in {} ms",
                took.as_millis()
            );
        }

        // The likely type of the object before a `.` — `vec.` means "the type of `vec`".
        for needle in asked.iter().filter(|needle| needle.ends_with('.')) {
            let Some(at) = text.find(needle.as_str()) else {
                continue;
            };
            let Some(reading) = view.reading_offset_of(FileOffset::new(at)) else {
                continue;
            };
            let step = Instant::now();
            let found = session.type_at(&view, reading);
            let took = step.elapsed();
            let said = match &found {
                Known::Yes(found) => found.type_of.clone(),
                other => format!("{other:?}"),
            };
            println!("   type at `{needle}`: {said} in {} ms", took.as_millis());
        }
    }
}
