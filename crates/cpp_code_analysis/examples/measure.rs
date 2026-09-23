//! Measuring the two numbers `docs/index-design.md` says to measure first: what a summary costs to build, and
//! what a cache hit costs.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example measure -- [file.cpp] [iterations] [-I dir]...
//! ```
//!
//! With no file it writes a synthetic translation unit of a few thousand lines into the temporary directory, so
//! the example runs anywhere. The point is the **ratio**: a build is a parse, a scope walk, a fact sweep and an
//! encode; a hit is a read and a decode. If a hit is not dramatically cheaper, the key has grown a component that
//! cannot be computed from the text, and the cache is back to paying for a parse before it can look anything up —
//! which is the mistake `cache.rs` records the removal of.
//!
//! A file whose includes do not resolve is not stored at all (`index::store`'s one rule about the filesystem), and
//! the example says so rather than reporting a hit rate of zero: pass `-I` for the headers it needs.

use std::path::PathBuf;

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut iterations = 20;
    let mut config = cpp_code_analysis::CompilerConfig::default();

    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if let Some(directory) = argument.strip_prefix("-I") {
            let directory = if directory.is_empty() {
                arguments.next().unwrap_or_default()
            } else {
                directory.to_string()
            };
            config = config.with_include_path(directory);
        } else if let Ok(count) = argument.parse() {
            iterations = count;
        } else {
            path = Some(PathBuf::from(argument));
        }
    }

    let root = std::env::temp_dir().join("cppls-measure");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a working directory");

    let (path, synthetic) = match path {
        Some(path) => (path, false),
        None => {
            // Two files, because a fixture whose includes do not resolve is a fixture whose summary is *never*
            // stored, and the measurement would end up comparing a build against another build. The header sits
            // beside the source, which is how a quoted include resolves.
            let header = root.join("synthetic.h");
            let source = root.join("synthetic.cpp");
            std::fs::write(&header, "struct Neighbour { int x; };\n").expect("the fixture writes");
            std::fs::write(&source, synthetic_unit(2_000)).expect("the fixture writes");
            (source, true)
        }
    };

    let source = std::fs::read_to_string(&path).expect("the file reads");
    let store_root = root.join("cache");

    let mut store = cpp_code_analysis::SummaryStore::open(&store_root, config.clone());
    let started = std::time::Instant::now();
    // Cloned, because the borrow of the store would otherwise last as long as the summary — and the stats below
    // need the store back. The clone is outside every timing below.
    let summary = store.get(&path).expect("the file reads").clone();
    let build = started.elapsed();
    let (declarations, macros, includes) = (
        summary.declarations.len(),
        summary.macros.len(),
        summary.includes.len(),
    );

    println!(
        "{} ({} bytes, {declarations} declarations, {macros} macros, {includes} includes)",
        if synthetic {
            "synthetic translation unit".to_string()
        } else {
            path.display().to_string()
        },
        source.len()
    );
    println!("  build (first get)   {build:>12?}");

    if store.stats().unstored > 0 {
        let unresolved: Vec<&str> = summary
            .includes
            .iter()
            .filter(|include| include.resolved.is_none())
            .map(|include| include.spelling.as_str())
            .collect();

        println!(
            "  hit                 {:>12}   this file is not cached: its includes do not resolve",
            "n/a"
        );
        println!("                      {unresolved:?} — pass -I for the directories they live in");
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    let mut hits = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        // A store of its own each round, because a store that has already answered holds the summary in memory;
        // what is being timed is the disk lookup, which is what a restart pays.
        let mut reopened = cpp_code_analysis::SummaryStore::open(&store_root, config.clone());
        let started = std::time::Instant::now();
        reopened.get(&path).expect("the file reads");
        hits.push(started.elapsed());
        assert_eq!(
            reopened.stats().reused,
            1,
            "every round must be a hit, or the fixture is measuring two builds: {:?}",
            reopened.stats()
        );
    }

    let total: std::time::Duration = hits.iter().sum();
    let mean = total / hits.len() as u32;

    println!(
        "  hit (mean of {iterations:>3}) {mean:>12?}   {:.1}x cheaper",
        build.as_secs_f64() / mean.as_secs_f64().max(f64::EPSILON)
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A file of the shape a real one has: an include, macros with guards, and a class per section.
fn synthetic_unit(sections: usize) -> String {
    let mut source = String::from("#pragma once\n#include \"synthetic.h\"\n\n");
    source.push_str("#if defined(PLATFORM_WINDOWS)\n#define API __declspec(dllexport)\n#else\n#define API\n#endif\n\n");

    for index in 0..sections {
        source.push_str(&format!(
            "namespace section_{index} {{\n\n\
             #define LIMIT_{index} {index}\n\n\
             struct API Widget{index} {{\n\
             \x20 int size;\n\
             \x20 const char* name;\n\
             \x20 Neighbour neighbour;\n\
             \x20 int compute(int factor) const;\n\
             }};\n\n\
             enum class Kind{index} {{ One, Two, Three }};\n\n\
             int helper_{index}(const Widget{index}& widget, int factor);\n\n\
             }}  // namespace section_{index}\n\n"
        ));
    }

    source
}
