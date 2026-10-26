//! **What reading the whole project costs** — one program per translation unit, in the background.
//!
//! §7.6's claim is that a project becomes queryable *root by root*: each translation unit is rendered and parsed once
//! as one program, and every file it covers gets the compiler's reading of it (⑲ files them per program). This probe
//! measures that on a real project rather than arguing about it: how many roots there are, how long they take, and
//! how much of the project ends up with a rendering.
//!
//! `CPPLS_PROJECT_ROOT` names the project; it defaults to the one this repository's probes use.

use std::time::Instant;

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    file::coordinates::FileOffset,
};

fn project() -> std::path::PathBuf {
    std::env::var_os("CPPLS_PROJECT_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\zc\Desktop\github\clice"))
}

/// Every source the project holds, by extension — what a client's workspace folder scan hands the session.
fn sources_of(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(directory: &std::path::Path, into: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|name| name == ".git" || name == "build")
                {
                    continue;
                }
                walk(&path, into);
            } else if path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().as_ref(),
                    "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx"
                )
            }) {
                into.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort();
    found
}

#[test]
#[ignore = "a probe: opens a real project and reads every translation unit, printing instead of asserting"]
fn what_reading_the_whole_project_costs() {
    let root = project();
    let all = sources_of(&root);
    let entry = all
        .iter()
        .find(|path| path.ends_with("src/AST/Selection.cpp"))
        .cloned()
        .unwrap_or_else(|| {
            all.iter()
                .find(|path| path.extension().is_some_and(|e| e == "cpp"))
                .expect("a source")
                .clone()
        });

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(all.iter().cloned());
    let text = std::fs::read_to_string(&entry).unwrap_or_default();
    session.did_open(&entry, &text);

    let started = Instant::now();
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    println!(
        "indexed {} file(s) in {:?}; {} cooked file(s)",
        all.len(),
        started.elapsed(),
        session.cache_sizes().cooked_files
    );

    // The project clock: the first program nobody is looking at waits ten seconds for a pause that is clearly not
    // mid-sentence. Nothing tells a session that time passed, so the probe waits.
    std::thread::sleep(std::time::Duration::from_millis(10_500));

    let reading = Instant::now();
    let mut roots = 0usize;
    let mut files = 0usize;
    let mut first: Option<std::time::Duration> = None;
    // **Until nothing is owed, not until a read succeeds.** A declined read is retried (three times), so a loop that
    // stopped on the first `None` would have left the probe's own project half-read — which is exactly what it did
    // before this line was written.
    while session.the_wait_before_a_program_may_be_read().is_some() {
        match session.read_a_program_now() {
            Some(read) => {
                if first.is_none() {
                    first = Some(reading.elapsed());
                }
                roots += 1;
                files += read.files;
            }
            None => break,
        }
    }
    println!(
        "{roots} program(s) read in {:?} (first {:?}), covering {files} file-read(s)",
        reading.elapsed(),
        first.unwrap_or_default()
    );

    let sizes = session.cache_sizes();
    println!(
        "afterwards: {} cooked file(s) / {} declaration(s), {} summar(ies)",
        sizes.cooked_files, sizes.cooked_declarations, sizes.summaries
    );

    // **Which files the project read did not reach**, by name: the ones still answering from their own text, and the
    // whole of what is left before the lookup-time guard patch can be deleted (§8.1).
    let uncovered: Vec<String> = session
        .index()
        .summaries()
        .filter(|summary| session.index().cooked_declarations(&summary.path).is_none())
        .map(|summary| summary.path.to_string_lossy().to_string())
        .collect();
    println!("{} file(s) have no rendering:", uncovered.len());
    for path in uncovered.iter().take(40) {
        let in_project = all
            .iter()
            .any(|said| said.to_string_lossy() == path.as_str());
        println!(
            "  {path}  (project file: {in_project}, dependents: {})",
            session
                .index()
                .dependents_of(std::path::Path::new(path))
                .len()
        );
    }
    assert!(roots > 0, "the project has at least one root");

    // One file at the end: is it answered from the compiler's reading?
    let probe = all
        .iter()
        .find(|path| path.ends_with("include/AST/Selection.h"))
        .cloned()
        .or_else(|| {
            all.iter()
                .find(|path| path.extension().is_some_and(|e| e == "h"))
                .cloned()
        });
    if let Some(header) = probe {
        let view = session.view(&header).expect("the header reads");
        println!(
            "a header nobody opened: {} cooked declaration(s), answers from {}",
            session
                .index()
                .cooked_declarations(&header)
                .map_or(0, <[cpp_code_analysis::DeclFact]>::len),
            if session.index().cooked_declarations(&header).is_some() {
                "the rendering"
            } else {
                "its own text"
            }
        );
        drop(view);
        let _ =
            session.member_completions(&session.view(&header).expect("reads"), FileOffset::new(10));
    }
    let _ = CompilerConfig::default();
    let _: Option<MemoryFiles> = None;
}
