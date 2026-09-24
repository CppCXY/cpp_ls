//! Temporary probe: what would indexing a standard-library closure cost?
//!
//! `cargo run --release -p cpp_code_analysis --example std_index -- <file-with-one-path-per-line> <cache-dir>`

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let cache = std::path::PathBuf::from(std::env::args().nth(2).expect("a cache directory"));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).expect("the cache directory");

    let paths: Vec<std::path::PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(std::path::PathBuf::from)
        .collect();

    let key = cpp_code_analysis::SummaryKey::new(0, 0);
    let mut summaries = Vec::new();

    let started = std::time::Instant::now();
    for path in &paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        summaries.push(cpp_code_analysis::summarize(path, &source, key));
    }
    let built = started.elapsed();

    let declared: usize = summaries.iter().map(|s| s.declarations.len()).sum();
    let macros: usize = summaries.iter().map(|s| s.macros.len()).sum();

    // The aliases, counted the way the query layer recognises them: a **type** fact with a `type_of` is a
    // `typedef`/`using` and the field is what it points at; a class is a type fact without one. Printed because
    // `docs/roadmap.md` §3.1 asks for the number — following an alias one step is what makes the standard
    // library's names (`std::string`, `std::vector`, every `*_type`) reachable at all, so how many there are is
    // how much of the surface that step is holding up.
    let aliases: usize = summaries
        .iter()
        .flat_map(|summary| &summary.declarations)
        .filter(|fact| fact.kind == cpp_code_analysis::DeclKind::Type && fact.type_of.is_some())
        .count();
    let types: usize = summaries
        .iter()
        .flat_map(|summary| &summary.declarations)
        .filter(|fact| fact.kind == cpp_code_analysis::DeclKind::Type)
        .count();
    let inherited: usize = summaries
        .iter()
        .flat_map(|summary| &summary.declarations)
        .filter(|fact| !fact.bases.is_empty())
        .count();

    let started = std::time::Instant::now();
    let mut bytes = 0usize;
    let mut encoded = Vec::new();
    for summary in &summaries {
        let file =
            cpp_code_analysis::write_summary(summary, &cache).expect("the summary writes");
        bytes += std::fs::metadata(&file)
            .map(|meta| meta.len() as usize)
            .unwrap_or(0);
        encoded.push(file);
    }
    let written = started.elapsed();

    let started = std::time::Instant::now();
    let mut read_back = 0usize;
    for file in &encoded {
        if cpp_code_analysis::read_summary(file).is_ok() {
            read_back += 1;
        }
    }
    let read = started.elapsed();

    println!(
        "files {} | declarations {declared} | macros {macros}\n\
         types {types} ({aliases} aliases, {} classes) | classes with bases {inherited}\n\
         build {:?} | encode+write {:?} | read+decode {:?} | on disk {} KB ({read_back} read back)",
        summaries.len(),
        types - aliases,
        built,
        written,
        read,
        bytes / 1024,
    );
}

