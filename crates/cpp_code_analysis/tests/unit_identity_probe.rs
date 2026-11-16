//! **A probe, not a test**: how much of one program's environment is another program's.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_code_analysis --test unit_identity_probe -- --ignored --nocapture
//! ```
//!
//! # The question
//!
//! The cache rewrite keys a file's rendering by **its text and its environment**:
//! `(content_hash, environment_fingerprint)`. The number that decides whether that is worth doing is not how many
//! files two programs share — it is how many of those shared files are shared **under the same environment**, because
//! that is the set that can be read once and reused. `MacroView::environment_fingerprint` hashes the closure of the
//! definitions the file's own names can reach, so the two answers differ whenever a program defines something the
//! header uses before including it.
//!
//! # What it prints
//!
//! ```text
//! per root     frames, distinct files, distinct keys
//! shared       files in both closures, of which how many agree on the key
//! disagreements  the files that differ, and why they are expected to (a macro defined before the include)
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

fn entries(root: &Path) -> Vec<PathBuf> {
    std::env::var("CPPLS_UNIT_ENTRIES")
        .map(|list| {
            list.split(';')
                .filter(|entry| !entry.is_empty())
                .map(|entry| root.join(entry))
                .collect()
        })
        .unwrap_or_else(|_| vec![root.join("src/containers.cpp"), root.join("src/basics.cpp")])
}

/// **One program's per-file keys** — `path -> (content hash, environment fingerprint)`.
type Keys = HashMap<PathBuf, (u64, u64)>;

/// One program's per-file keys: `path -> (content hash, environment fingerprint)`.
fn keys_of(session: &Session, root: &Path) -> Keys {
    let Some(unit) = session.translation_unit_of(root) else {
        println!("{}: no unit", root.display());
        return Keys::new();
    };
    let mut keys = Keys::new();
    for file in unit.files() {
        let Some(view) = unit.environment_of(file) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let content = cpp_code_analysis::content_hash(&text);
        keys.insert(
            file.to_path_buf(),
            (content, view.environment_fingerprint(&text, &tokens)),
        );
    }
    keys
}

#[test]
#[ignore = "a probe: opens a real project and prints instead of asserting"]
fn how_much_of_one_program_is_another_programs() {
    let root = project();
    let entries = entries(&root);
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(entries.clone());
    session.index_everything();
    let mut slices = 0;
    while session.pending_work() > 0 && slices < 10_000 {
        session.advance(256);
        slices += 1;
    }

    let mut per_root: Vec<(PathBuf, Keys)> = Vec::new();
    for entry in &entries {
        let started = std::time::Instant::now();
        let keys = keys_of(&session, entry);
        println!(
            "{}: {} file(s), {} distinct key(s), {:?}",
            entry.display(),
            keys.len(),
            keys.values()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            started.elapsed(),
        );
        per_root.push((entry.clone(), keys));
    }

    let (Some((left_root, left)), Some((right_root, right))) = (per_root.first(), per_root.get(1))
    else {
        return;
    };
    let shared: Vec<&PathBuf> = left
        .keys()
        .filter(|path| right.contains_key(*path))
        .collect();
    let agreeing: Vec<&&PathBuf> = shared
        .iter()
        .filter(|path| left.get(**path) == right.get(**path))
        .collect();
    println!(
        "{} vs {}: {} file(s) in both, {} of them with the same text and environment",
        left_root.display(),
        right_root.display(),
        shared.len(),
        agreeing.len()
    );
    let mut differing: Vec<&&PathBuf> = shared
        .iter()
        .filter(|path| left.get(**path) != right.get(**path))
        .collect();
    differing.sort();
    for path in differing.iter().take(12) {
        println!(
            "  differs: {}",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        );
    }
    let unique: usize = per_root.iter().map(|(_, keys)| keys.len()).sum::<usize>() - shared.len();
    let distinct = per_root
        .iter()
        .flat_map(|(_, keys)| keys.values().copied())
        .collect::<std::collections::HashSet<_>>()
        .len();
    println!(
        "{unique} per-file reading(s) belong to one program only, and the two together need {distinct} key(s)"
    );
}
