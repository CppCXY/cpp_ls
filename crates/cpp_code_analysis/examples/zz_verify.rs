//! TEMPORARY: what a real session now reads for `<format>`. Deleted before the round ends.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = root.join("main.cpp");

    let files = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::open(root.clone(), files, WatchFilter::new(&root));
    session.index_everything();

    let standard_library = session
        .config()
        .system_include_paths()
        .next()
        .map(std::path::Path::to_path_buf)
        .expect("the standard library's directory");

    for header in ["format", "string", "cstdio"] {
        let path = standard_library.join(header);
        let Some(summary) = session.index().summary(&path) else {
            println!("--- <{header}>: not in the index ---");
            continue;
        };
        let std_count = summary
            .declarations
            .iter()
            .filter(|fact| fact.scope.as_deref() == Some("std"))
            .count();
        println!(
            "--- <{header}>: {} declarations, {std_count} scoped to `std`, {} macro readings",
            summary.declarations.len(),
            summary.macro_readings.len()
        );
    }

    println!(
        "\ndeclarations_in(\"std\") = {}",
        session.index().declarations_in("std", &file).len()
    );

    // **Is `<format>` reachable from the project's file at all?** The declaration is in the index and it says
    // `std::format`, so the only thing between that and a lookup is the visibility walk.
    if let Some(summary) = session.index().summary(&file) {
        println!("main.cpp's includes:");
        for include in &summary.includes {
            println!(
                "  {:?} {:?} -> {:?}",
                include.spelling,
                include.form,
                include.resolved.as_ref().map(|path| path.display().to_string())
            );
        }
    }

    let visible = session.index().visible_files(&file);
    println!("visible from main.cpp: {} files", visible.len());
    for (path, visibility) in &visible {
        if path.contains("format") {
            println!("  {visibility:?} {path}");
        }
    }

    for name in ["std::format", "std::printf", "std::string"] {
        let direct = session.index().files_declaring(name, &file);
        println!(
            "files_declaring({name:<12}) = {} {:?}",
            direct.len(),
            direct
                .iter()
                .take(4)
                .map(|found| format!("{} @ {:?}", found.fact.qualified_name(), found.visibility))
                .collect::<Vec<_>>()
        );
        println!(
            "definition({name:<12}) = {:?}",
            session
                .index()
                .definition(name, &file)
                .value()
                .map(|found| found.fact.qualified_name())
        );
    }

    // The facts themselves, asked of the summary rather than through the walk.
    let format_path = standard_library.join("format");
    if let Some(summary) = session.index().summary(&format_path) {
        let facts: Vec<String> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name == "format")
            .map(|fact| format!("{} local {} clean {}", fact.qualified_name(), fact.local, fact.clean))
            .collect();
        println!("`format` facts in <format>: {facts:?}");
    }
}
