//! Indexing a file **and everything it includes**, cold and then warm — the numbers P0 and P2 are about.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example index_includes
//! ```
//!
//! It writes a small project into the temporary directory whose `main.cpp` includes a local header and
//! `<vector>`, discovers the toolchain on this machine, and indexes the closure twice: once with an empty cache
//! and once with the cache the first run wrote. The second run is the one that says whether the design works —
//! it should parse nothing at all.
//!
//! The standard-library part of the closure is the interesting half, and it is why this exists as a measurement
//! rather than as a test: the shape of the answer depends on the toolchain installed, and `docs/std-library.md`
//! records what it looked like on the machine this was written on.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CompilerConfig, DiskCommands, DiskFiles, Environment, IncludeBudget, Known, SummaryStore,
    discover,
};

fn main() {
    let root = std::env::temp_dir().join("cppls-index-includes");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");

    let main = root.join("main.cpp");
    std::fs::write(
        &main,
        "#include \"widget.h\"\n#include <vector>\n#include <string>\n\
         int main() { std::vector<std::string> names; return 0; }\n",
    )
    .expect("the fixture writes");
    std::fs::write(
        root.join("widget.h"),
        "struct Widget {\n  int size;\n  void grow();\n};\n",
    )
    .expect("the fixture writes");

    let files = DiskFiles;
    let toolchain = discover(
        &files,
        &DiskCommands,
        None,
        &main,
        &Environment::current(),
    );

    let config = match &toolchain {
        Some(toolchain) => {
            println!(
                "toolchain: {} ({})\n  {} system include directories",
                toolchain.compiler.display(),
                toolchain.version.as_deref().unwrap_or("version not reported"),
                toolchain.system_include_paths.len()
            );
            toolchain.config(&CompilerConfig::new())
        }
        None => {
            println!(
                "no compiler was found, so `<vector>` will not resolve — the closure will be the project's own \
                 files and nothing else"
            );
            CompilerConfig::new()
        }
    };

    let cache = root.join(".cppls");

    // --- cold -------------------------------------------------------------------------------------
    let started = std::time::Instant::now();
    let cold = {
        let mut store = SummaryStore::open(&root, config.clone());
        store.index_includes_from(&main, IncludeBudget::default())
    };
    let cold_took = started.elapsed();

    println!(
        "\ncold: {} files in {cold_took:?} — {} parsed, {} from disk, {} not stored",
        cold.indexed.len(),
        cold.stats.rebuilt,
        cold.stats.reused,
        cold.stats.unstored
    );
    println!("  {} unresolved includes", cold.unresolved.len());
    for edge in cold.unresolved.iter().take(5) {
        println!("    {} :: {}", edge.from.display(), edge.spelling);
    }
    if !cold.not_indexed.is_empty() {
        println!("  {} files were reached and not opened:", cold.not_indexed.len());
        for stopped in cold.not_indexed.iter().take(5) {
            println!("    {} :: {:?}", stopped.path.display(), stopped.reason);
        }
    }
    println!("  cache: {} KB", directory_size(&cache) / 1024);

    // --- warm -------------------------------------------------------------------------------------
    let started = std::time::Instant::now();
    let mut store = SummaryStore::open(&root, config);
    let warm = store.index_includes_from(&main, IncludeBudget::default());
    let warm_took = started.elapsed();

    println!(
        "\nwarm: {} files in {warm_took:?} — {} parsed, {} from disk",
        warm.indexed.len(),
        warm.stats.rebuilt,
        warm.stats.reused
    );

    // --- and the point of it ----------------------------------------------------------------------
    let found = store.index().definition("Widget", &main);
    println!(
        "\na query after the walk: `Widget` is {}",
        match &found {
            Known::Yes(found) => format!("declared in {}", found.file.display()),
            Known::Unknown(reason) => reason.describe(),
            Known::No => "definitely not declared".to_string(),
        }
    );

    match store.index().definition("string", &main) {
        Known::Yes(found) => println!(
            "and `string` — a standard-library name two headers down the include graph — is declared in {}",
            found.file.display()
        ),
        Known::Unknown(reason) => {
            println!("and `string`: {}", reason.describe());
        }
        Known::No => println!("and `string` is definitely not declared"),
    }

    println!("\nproject: {}", root.display());
}

/// The size of everything under a directory, in bytes.
///
/// The one thing a caller wants to know about a cache it cannot see into, and there is no `du` worth shelling out
/// for.
fn directory_size(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let path: PathBuf = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => directory_size(&path),
                Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                Err(_) => 0,
            }
        })
        .sum()
}
