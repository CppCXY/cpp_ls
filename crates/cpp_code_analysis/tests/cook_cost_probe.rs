//! **What one cook costs with S1 landed** — the number `docs/module-cache.md` promises: a file's cook no longer walks
//! its closure, so the time is the file's own (lex, render, parse) rather than the program's.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! $env:CPPLS_COOK_FILE = "CodeFormat/src/LuaCheck.cpp"
//! cargo test --release -p cpp_code_analysis --test cook_cost_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::include::config::CompilerConfig;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: cooks one file of a real project and reports the numbers"]
fn what_one_cook_costs() {
    let root = PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    );
    let relative = std::env::var("CPPLS_COOK_FILE")
        .unwrap_or_else(|_| "CodeFormat/src/LuaCheck.cpp".to_string());
    let file = root.join(&relative);

    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files([file.clone()]);
    let indexing = Instant::now();
    session.index_everything();
    println!(
        "index        {:>8.1} ms, walked {} unit(s)",
        indexing.elapsed().as_secs_f64() * 1000.0,
        session.unit_stats().walked
    );

    for round in 1..=3 {
        let started = Instant::now();
        let cooked = session.cook(&file);
        let took = started.elapsed();
        println!(
            "cook {round}       {:>8.1} ms | {} | walked {}",
            took.as_secs_f64() * 1000.0,
            match cooked {
                Some(reading) => format!("{} declaration(s)", reading.declarations),
                None => "declined".to_string(),
            },
            session.unit_stats().walked,
        );
    }
}
