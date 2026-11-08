//! **The include walk's own cost** — a probe, not a test.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test walk_probe -- --ignored --nocapture
//! ```
//!
//! `include::graph::walk` used to parse **every file it entered**: a header reached through an `#include` was parsed
//! only so its directives could be read out of the tree. It now reads them from the token stream (`Directives`), and
//! this measures the walk alone — no summaries, no cooking, no index — so the change is judged on the axis it claims
//! to move. `closure_probe` is the hand-written version of the same walk (lex + directives, its own resolver); this
//! one is the real function, with the real resolver and the real guard analysis.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::config::CompilerConfig;
use cpp_code_analysis::include::config::IncludePath;
use cpp_code_analysis::include::graph::walk;
use cpp_code_analysis::paths::{DiskFiles, PathInterner};

const MSVC: &str =
    r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215";
const SDK: &str = r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.22621.0";

fn configuration() -> CompilerConfig {
    let mut config = CompilerConfig::default();
    for directory in [
        Path::new(MSVC).join("include"),
        Path::new(SDK).join("ucrt"),
        Path::new(SDK).join("shared"),
        Path::new(SDK).join("um"),
        Path::new(SDK).join("winrt"),
    ] {
        config.include_paths.push(IncludePath::user(directory));
    }
    config
}

#[test]
#[ignore = "reads the toolchain's own headers; run with --ignored"]
fn the_walk_costs_what() {
    let root: PathBuf = Path::new(MSVC).join("modules").join("std.ixx");
    if !root.is_file() {
        eprintln!("skipped: no std.ixx on this machine");
        return;
    }
    let Ok(source) = std::fs::read_to_string(&root) else {
        eprintln!("skipped: the module source is unreadable");
        return;
    };
    let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
    let config = configuration();
    let files = DiskFiles;
    let mut interner = PathInterner::new(cfg!(windows));

    let started = Instant::now();
    let graph = walk(&source, &tree, &root, &files, &config, &mut interner);
    let elapsed = started.elapsed();

    println!(
        "walk of `std.ixx`: {} file(s) analysed, {} include edge(s) in {:?} ({:.2} ms/file)",
        graph.analysed.len(),
        graph.edges.len(),
        elapsed,
        elapsed.as_secs_f64() * 1000.0 / graph.analysed.len().max(1) as f64,
    );
    let unresolved = graph.edges.iter().filter(|_edge| false).count();
    println!("  {} unresolved include(s)", unresolved);
}
