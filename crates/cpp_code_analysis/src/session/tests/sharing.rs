//! **How many of a project's files share an environment?** — the number the reachable record key is judged by.
//!
//! A record keyed by `the_closure_fingerprint` is shared only when two files' closures are byte-identical, which for
//! translation units in one project is essentially never. A record keyed by the macros **in force** (the environment
//! fingerprint) is shared whenever two files see the same macros — which is the question this probe answers before any
//! such key is built: if one project's files do not share environments, the key buys no sharing and the item is not
//! worth the composition it puts on the hit path.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test -p cpp_code_analysis --lib session::tests::sharing -- --ignored --nocapture
//! ```
//!
//! **This is a probe, not a test**: it says nothing about correctness and is `#[ignore]`d so the suite never pays for
//! indexing a real project.

use crate::file::paths::DiskFiles;
use crate::include::config::CompilerConfig;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: indexes a real project and reports how many of its files share a closure and an environment"]
pub(crate) fn how_many_files_share_an_environment() {
    let Ok(root) = std::env::var("CPPLS_REAL_PROJECT") else {
        eprintln!("set CPPLS_REAL_PROJECT to run this probe");
        return;
    };
    let root = std::path::PathBuf::from(root);
    let directories = std::env::var("CPPLS_PROBE_DIRS").unwrap_or_else(|_| {
        "CodeFormat,CodeFormatCLib,CodeFormatCore,CodeFormatLib,CodeFormatServer,LuaParser,Util"
            .to_string()
    });
    let mut sources = Vec::new();
    for directory in directories.split(',') {
        let mut pending = vec![root.join(directory.trim())];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| {
                    extension == "cpp" || extension == "cc" || extension == "cxx"
                }) {
                    sources.push(path);
                }
            }
        }
    }
    sources.sort();
    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files(sources.clone());
    session.index_everything();

    let mut closures: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut environments: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut counted = 0usize;
    for path in &sources {
        let (Some(closure), Some(environment)) = (
            session.store.the_closure_fingerprint(path),
            session.store.the_indexed_environment(path),
        ) else {
            continue;
        };
        closures.insert(closure);
        environments.insert(environment.fingerprint());
        counted += 1;
    }
    println!(
        "{counted} file(s): {} distinct closure(s), {} distinct environment(s)",
        closures.len(),
        environments.len()
    );
}
