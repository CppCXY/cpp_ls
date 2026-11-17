//! **How much of the closure a name query can see** — the measurement behind "`std::to_string` has no hover".
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_code_analysis --test visibility_probe -- --ignored --nocapture
//! ```
//!
//! # The report
//!
//! For the entry file it prints the size of the closure the index walks for a name query, then asks a few names the
//! file plainly uses and says what came back. A name that is in the index — `inspect facts --file bits/basic_string.h`
//! lists nine `to_string` overloads — and is *not* an answer here is a visibility defect, not a reading defect: the
//! walk that decides which files a query looks at stopped before the file that declares them.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

#[test]
#[ignore = "a probe: opens a real project and prints instead of asserting"]
fn what_a_name_query_can_see() {
    let root = project();
    let entry = root.join(
        std::env::var("CPPLS_VISIBILITY_FILE").unwrap_or_else(|_| "src/containers.cpp".to_string()),
    );
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([entry.clone()]);
    session.index_everything();
    let text = std::fs::read_to_string(&entry).expect("the entry file is readable");
    session.did_open(&entry, &text);

    // **The pump, which is what reads the closure** — the same loop the server runs, stopped when it stops making
    // progress (a project whose queue cannot drain would otherwise keep this probe here for ever).
    let started = Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 400 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "pumped {slices} slice(s) in {:?}, {} work item(s) left",
        started.elapsed(),
        session.pending_work()
    );

    let visible = session.index().visible_files_with_modules(&entry);
    println!("closure: {} file(s)", visible.0.len());
    let wanted = [
        "iostream",
        "string",
        "bits/basic_string.h",
        "bits/stl_vector.h",
    ];
    for tail in wanted {
        let found = visible.0.iter().any(|(path, _)| path.ends_with(tail));
        println!(
            "  {tail:<24} {}",
            if found { "visible" } else { "NOT VISIBLE" }
        );
    }

    for name in [
        "std::cout",
        "std::to_string",
        "std::string",
        "std::vector",
        "std::basic_string",
        "substr",
        "to_string",
    ] {
        let started = Instant::now();
        let answer = session.index().definitions(name, &entry);
        let shown = match &answer {
            cpp_code_analysis::Known::Yes(found) => format!(
                "{} declaration(s), first in {}",
                found.found.len(),
                found
                    .found
                    .first()
                    .map(|one| Path::new(&one.file)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string())
                    .unwrap_or_default()
            ),
            cpp_code_analysis::Known::Unknown(reason) => format!("unknown — {reason:?}"),
            cpp_code_analysis::Known::No => "no declaration".to_string(),
        };
        println!(
            "{name:<20} {:>8.1} ms  {shown}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
