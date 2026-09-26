//! The **closure of one translation unit**, as a file list and as a bill of what stopped it.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example std_closure -- <entry.cpp> [--list <out.txt>]
//! ```
//!
//! # Why this exists
//!
//! The libstdc++ corpus the census runs over was produced by the compiler itself — `g++ -M -std=c++20 t.cpp`
//! prints the closure and nothing else has to be trusted. MSVC has no such switch: `cl /showIncludes` writes
//! *localized* text (`注意: 包含文件:`) that a script would have to match by hand, and the reading of that text is
//! exactly the kind of guess this project refuses to make about a compiler's output.
//!
//! So the closure is asked of **the engine's own walk** instead — the same [`SummaryStore::index_includes_from`]
//! that answers a query, over the same toolchain discovery a session uses. That makes the list a measurement of
//! what the index *actually reached*, which is a stronger claim than what the compiler would have read: a header
//! the walk never opened is a header no query can see, and the two lists differing is a finding rather than a
//! convenience.
//!
//! # What it prints
//!
//! 1. the toolchain that answered, and the closure's size;
//! 2. the files it reached and did not open, with the reason ([`NotIndexedReason`]) — a budget that truncated
//!    silently would make a partial closure look like a whole one;
//! 3. every `#include` that resolved to nothing, which is the one thing that stops the walk dead;
//! 4. with `--list`, the paths themselves, one per line, ready to feed a census.
//!
//! # What it does not do
//!
//! It does not decide anything about the files it lists. It exists so that a **census** can be run over a
//! toolchain's own standard library — `std_probe --seeds --closure` is the other half — and the numbers that come
//! back are per-file and re-measurable after every change to a reading.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CompilerConfig, DiskCommands, DiskFiles, Environment, IncludeBudget, SummaryStore, discover,
};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(entry) = arguments.next() else {
        eprintln!("usage: std_closure <entry.cpp> [--list <out.txt>]");
        std::process::exit(2);
    };

    let mut list: Option<PathBuf> = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--list" => list = arguments.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }

    let entry = std::fs::canonicalize(Path::new(&entry)).unwrap_or_else(|_| PathBuf::from(&entry));
    let root = std::env::temp_dir().join("cppls-std-closure");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");

    let files = DiskFiles;
    let toolchain = discover(
        &files,
        &DiskCommands,
        None,
        &entry,
        &Environment::current(),
        &cpp_code_analysis::include::msvc::WindowsLayout::current(),
    );

    let config = match &toolchain {
        Some(toolchain) => {
            println!(
                "toolchain: {} ({})\n  {} system include directories",
                toolchain.compiler_name(),
                toolchain.version.as_deref().unwrap_or("version not reported"),
                toolchain.system_include_paths.len()
            );
            toolchain.config(&CompilerConfig::new())
        }
        None => {
            println!("no compiler was found: the closure will be the project's own files and nothing else");
            CompilerConfig::new()
        }
    };

    let started = std::time::Instant::now();
    // A cache directory of its own under the temporary directory, so a run is cold and the numbers are the walk's
    // rather than the disk's. `include::Index::cache_dir` in a `.cppls.toml` is where a project puts the real one.
    let mut store = SummaryStore::open(&root, config);
    let index = store.index_includes_from(&entry, IncludeBudget::default());
    let took = started.elapsed();

    println!(
        "\nclosure: {} files in {took:?} — {} parsed, {} from disk, {} not stored",
        index.indexed.len(),
        index.stats.rebuilt,
        index.stats.reused,
        index.stats.unstored
    );

    if index.unresolved.is_empty() {
        println!("  every include resolved");
    } else {
        println!("  {} includes resolved to nothing:", index.unresolved.len());
        for edge in &index.unresolved {
            println!("    {} :: {}", edge.from.display(), edge.spelling);
        }
    }

    if index.not_indexed.is_empty() {
        println!("  every file the walk reached was opened");
    } else {
        println!("  {} files were reached and not opened:", index.not_indexed.len());
        for stopped in &index.not_indexed {
            println!("    {} :: {:?}", stopped.path.display(), stopped.reason);
        }
    }

    if let Some(list) = list {
        let mut text = String::new();
        for path in &index.indexed {
            text.push_str(&path.to_string_lossy().replace('\\', "/"));
            text.push('\n');
        }
        std::fs::write(&list, text).expect("the list writes");
        println!("\nwrote {} paths to {}", index.indexed.len(), list.display());
    }
}
