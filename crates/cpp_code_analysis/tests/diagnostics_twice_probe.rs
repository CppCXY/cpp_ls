//! **The diagnostic answer, asked twice** — the server's pull does exactly this, and the second one is where the
//! `containers.cpp` report's growth starts.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test diagnostics_twice_probe -- --ignored --nocapture
//! ```
//!
//! Measured in the server (`CPPLS_TRACE_ALLOCS`): the first pull's `diagnose_file` takes 5 ms, the second one never
//! returns — one blocking closure of `snapshot_query` started and no `compute done` followed it, with the process at
//! one core and ~90 MB/s. This probe asks the same question in process, with no server, so the loop can be bisected
//! from the outside.

use std::path::PathBuf;
use std::time::Instant;

fn project() -> PathBuf {
    std::env::var_os("CPPLS_TWICE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_cpp"))
}

#[test]
#[ignore = "a probe: reads a real project and asks the same question twice"]
fn the_same_diagnostic_answer_asked_twice() {
    let root = project();
    let file = root.join("src/containers.cpp");
    let text = std::fs::read_to_string(&file).expect("the reported file is here");
    let mut session = cpp_code_analysis::Session::open(
        root.clone(),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    let indexing = Instant::now();
    session.index_everything();
    eprintln!("[twice] indexed in {} ms", indexing.elapsed().as_millis());

    for round in 0..2 {
        let started = Instant::now();
        let answer = session.diagnostics(&file);
        eprintln!(
            "[twice] ask {round}: {} ms, {:?}",
            started.elapsed().as_millis(),
            answer.as_ref().map(|found| (
                found.reading,
                found.errors.len(),
                found.checks.len(),
                found.checks_withheld
            ))
        );
    }
}
