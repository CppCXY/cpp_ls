//! **A measurement of the UE-like project** — a simulated Unreal Engine module, and the two convention rules.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test ue -- --ignored --nocapture
//! $env:CPPLS_UE_CORPUS = "C:\Users\zc\Desktop\UE-like"   # the default
//! ```
//!
//! The directory holds a simulated `CoreMinimal.h` — the engine's macros expanded to nothing, its core types spelled the
//! way a game module spells them — plus two sources: one that follows the engine's conventions and one with a single
//! violation of each rule. The questions are the ones a reader of UE code asks:
//!
//! ```text
//! parse     does every file read without a syntax error, though there is no engine on this machine?
//! findings  is the clean file quiet, and does the bad one name exactly the conventions it breaks?
//! ```

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiagnosticCode, DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn corpus_root() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_UE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\UE-like"));
    named.join(".cppls.toml").is_file().then_some(named)
}

/// The findings of a file, as `(code, name)` pairs, in the order the checks produced them.
fn findings_of(session: &Session<DiskFiles>, path: &Path) -> Vec<(DiagnosticCode, String)> {
    session
        .diagnostics(path)
        .map(|found| {
            found
                .checks
                .iter()
                .map(|finding| (finding.code, finding.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "a measurement of a project on disk, like the corpora"]
fn the_ue_like_project() {
    let Some(root) = corpus_root() else {
        println!("no UE-like project at the default path, so nothing to measure");
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files: Vec<PathBuf> = session
        .project_files()
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "h" | "cpp"))
        })
        .cloned()
        .collect();
    session.add_project_files(files.clone());
    session.index_everything();
    let mut steps = 0usize;
    while session.pending_work() > 0 && steps < 20_000 {
        session.advance(64);
        steps += 1;
    }

    println!("\n--- one line per file");
    let mut by_file: std::collections::BTreeMap<String, Vec<(DiagnosticCode, String)>> =
        std::collections::BTreeMap::new();
    for path in &files {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        session.cook(path);
        let errors = session
            .diagnostics(path)
            .map(|found| found.errors.len())
            .unwrap_or(0);
        let findings = findings_of(&session, path);
        println!(
            "{name:<24} {errors} parse error(s), {} finding(s)",
            findings.len()
        );
        by_file.insert(name, findings);
    }

    println!("\n--- the convention findings");
    for (file, findings) in &by_file {
        for (code, name) in findings {
            if matches!(
                code,
                DiagnosticCode::UeNaming | DiagnosticCode::UeGenerateBody
            ) {
                println!("  {file}: {code:?} on `{name}`");
            }
        }
    }

    // **The clean file is quiet and the bad one names every violation**, which is the whole measurement: the rules are
    // conventions of the engine, not of C++, and the simulation makes a project that reads them possible.
    for name in ["Source/Game/Clean.h", "Source/Game/Clean.cpp"] {
        let findings = by_file.get(name).cloned().unwrap_or_default();
        let convention: Vec<&(DiagnosticCode, String)> = findings
            .iter()
            .filter(|(code, _)| {
                matches!(
                    code,
                    DiagnosticCode::UeNaming | DiagnosticCode::UeGenerateBody
                )
            })
            .collect();
        assert!(
            convention.is_empty(),
            "`{name}` follows the engine's conventions: {convention:?}"
        );
    }

    let bad = by_file
        .get("Source/Game/Bad.h")
        .cloned()
        .unwrap_or_default();
    let naming: Vec<&str> = bad
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::UeNaming)
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(
        naming,
        vec!["MyActor", "isReady", "MyData", "State"],
        "every violated prefix is reported, in document order: {bad:?}"
    );
    let missing: Vec<&str> = bad
        .iter()
        .filter(|(code, _)| *code == DiagnosticCode::UeGenerateBody)
        .map(|(_, name)| name.as_str())
        .collect();
    assert_eq!(
        missing,
        vec!["MyActor"],
        "the class without GENERATED_BODY() is reported: {bad:?}"
    );
}
