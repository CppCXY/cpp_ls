//! **How many of the timeline walks are the same walk, and which includes still do not resolve.**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example duplicate_closures -- <dir> [<entry.cpp>]
//! ```
//!
//! # The first question, and why it is asked before any more parallelism
//!
//! A cold index of `E:\EmmyLuaCodeStyle` walks **1188 timelines** and spends **46 s** of a 70 s index doing it
//! (`examples/project_index.rs`, `Session::walks_by_root`). The obvious move — run them on sixteen cores — was tried
//! and measured: it made the same index go from **67.3 s to 142.7 s**, because twenty-seven `advance` calls each
//! spawning sixteen threads, all contending for three mutexes inside `translation_unit_of`, is thread creation and
//! lock contention laid over work that already does its own file I/O.
//!
//! So before trying that again, the question is whether the walks are *independent* work at all:
//!
//! ```text
//!   if many roots produce the same closure, the answer is not to run those walks at once —
//!   it is to run one of them and share the result
//! ```
//!
//! # How closure identity is computed here, and what it means
//!
//! A timeline is a function of the closure's **content** and the compilation context, so two roots with the same
//! closure in the same context must produce the same timeline. That is the same reasoning
//! [`crate::tu_cache`]'s on-disk key uses — which is why a **warm** run reports `reused 1494 / rebuilt 235`: the
//! disk already shares what is shareable between runs. What this measures is the sharing available **within one
//! run**, which the disk cannot help with because nothing is on it yet.
//!
//! The identity is computed bottom-up over the include DAG rather than by materialising each closure: a file's
//! hash is its own path and content hash plus the **sorted** hashes of what it includes. Sorting is what makes two
//! files that include the same headers in a different order hash the same, which is correct — the closure is a set.
//! A cycle (legal in C++ behind guards) is broken with a sentinel rather than followed.
//!
//! # The second question
//!
//! `unstored` on that project was **235 → 153** after the rule about conditional includes was narrowed
//! (`index::store::has_unresolved_includes`). The 153 that remain are files with an **unconditional** include that
//! does not resolve — which is either a rule that is still too strict or an include path that is not configured, and
//! the two need different fixes. This prints which spellings they are, so the answer is read rather than guessed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, SummaryKey, WatchFilter};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: duplicate_closures <dir> [<entry.cpp>]");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = std::env::args().nth(2).map(PathBuf::from);

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    if let Some(entry) = &entry {
        let text = std::fs::read_to_string(entry).unwrap_or_default();
        session.did_open(entry, &text);
    }
    let started = std::time::Instant::now();
    // **The pump's own alternation, not `index_everything`.** The two are not the same job and the difference is a
    // factor of four and a half: `index_everything` alternates indexing a slice with **cooking** one, and cooking is
    // lex + expand + render + parse-the-rendering per file. Measured on this project, that is **311 s**; the
    // indexing alone is **70 s**, which is the number this probe is about. Naming a probe after a question it does
    // not ask is how a 4.5× gets read as a regression.
    while session.pending() > 0 {
        session.advance(64);
    }
    println!(
        "project: {} files indexed in {:?}",
        session.index().len(),
        started.elapsed()
    );

    // **Which files include what**, taken from the summaries: the resolved edges only, because an unresolved one
    // leads nowhere and is the subject of the second question.
    let mut includes: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut content: HashMap<PathBuf, SummaryKey> = HashMap::new();
    for summary in session.index().summaries() {
        let edges = summary
            .includes
            .iter()
            .filter_map(|include| include.resolved.clone())
            .collect();
        includes.insert(summary.path.clone(), edges);
        content.insert(summary.path.clone(), summary.key);
    }

    let mut hashes: HashMap<PathBuf, u64> = HashMap::new();
    let mut in_progress: Vec<PathBuf> = Vec::new();
    for path in includes.keys() {
        closure_hash(
            path,
            &includes,
            &content,
            &mut hashes,
            &mut in_progress,
        );
    }

    // **The group key is what a root reads underneath itself, not the root.** A file is part of its own closure, so
    // keying by the full closure would make every root distinct by construction — the first version of this probe
    // did exactly that and printed `distinct closures 2005 of 2005`, a number that says nothing at all. The question
    // is whether two roots pull in the same headers, in the same versions: that is what decides whether one walk
    // could answer for both, and it is this value.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let rest_of = |path: &PathBuf| -> u64 {
        let mut children: Vec<u64> = includes
            .get(path)
            .map(|edges| {
                edges
                    .iter()
                    .map(|child| hashes.get(child).copied().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        children.sort_unstable();
        let mut hash = OFFSET;
        for child in children {
            for byte in child.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(PRIME);
            }
        }
        hash
    };

    let mut groups: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for path in includes.keys() {
        groups.entry(rest_of(path)).or_default().push(path.clone());
    }

    let roots = hashes.len();
    let shared: Vec<(&u64, &Vec<PathBuf>)> = groups.iter().filter(|(_, files)| files.len() > 1).collect();
    let shareable: usize = shared.iter().map(|(_, files)| files.len() - 1).sum();

    println!("\n--- one closure, how many roots ---");
    println!("  roots                              {roots}");
    println!("  distinct closures                  {}", groups.len());
    println!(
        "  walks a shared result would remove {shareable}   ({:.1}%)",
        shareable as f64 * 100.0 / roots.max(1) as f64
    );

    let mut biggest: Vec<(&u64, &Vec<PathBuf>)> = shared;
    biggest.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    for (hash, files) in biggest.iter().take(8) {
        println!(
            "  {hash:016x}  {:5} roots  e.g. {}",
            files.len(),
            short(&files[0])
        );
    }

    // **The includes that still do not resolve and would block the write**, by spelling: only the ones whose guard
    // is `Unconditional`, which is exactly what `index::store::has_unresolved_includes` refuses a summary over. The
    // rest are conditional and are already cacheable — `unstored` on this project fell from 235 to 153 when that
    // rule was narrowed, and the 82 files between those two numbers are the ones this filter leaves out.
    let mut unresolved: HashMap<&str, usize> = HashMap::new();
    let mut files_affected = 0usize;
    for summary in session.index().summaries() {
        let missing: Vec<&str> = summary
            .includes
            .iter()
            .filter(|include| {
                include.resolved.is_none()
                    && matches!(include.guard, cpp_code_analysis::summary::FactGuard::Unconditional)
            })
            .map(|include| include.spelling.as_str())
            .collect();
        if missing.is_empty() {
            continue;
        }
        files_affected += 1;
        for spelling in missing {
            *unresolved.entry(spelling).or_default() += 1;
        }
    }

    let mut unresolved: Vec<(&&str, &usize)> = unresolved.iter().collect();
    unresolved.sort_by_key(|(_, count)| std::cmp::Reverse(**count));

    println!("\n--- unconditional includes that resolve to nothing (what blocks a write) ---");
    println!("  files affected                     {files_affected}");
    for (spelling, count) in unresolved.iter().take(20) {
        println!("  {count:5} file(s)  {spelling}");
    }
}

/// A file's closure identity: itself, plus the sorted identities of what it includes.
fn closure_hash(
    path: &Path,
    includes: &HashMap<PathBuf, Vec<PathBuf>>,
    content: &HashMap<PathBuf, SummaryKey>,
    hashes: &mut HashMap<PathBuf, u64>,
    in_progress: &mut Vec<PathBuf>,
) -> u64 {
    if let Some(hash) = hashes.get(path) {
        return *hash;
    }
    // A cycle is legal behind include guards, and breaking it with a sentinel rather than following it is what
    // keeps this a walk instead of a hang. Two files in a cycle then share that sentinel on the edge, which is
    // right: neither can be reached from the other without the guard having been taken.
    if in_progress.iter().any(|above| above == path) {
        return 0x9e37_79b9_7f4a_7c15;
    }
    in_progress.push(path.to_path_buf());

    let mut children: Vec<u64> = includes
        .get(path)
        .map(|edges| {
            edges
                .iter()
                .map(|child| closure_hash(child, includes, content, hashes, in_progress))
                .collect()
        })
        .unwrap_or_default();
    children.sort_unstable();

    in_progress.pop();

    // FNV-1a over the path, the content hash and the children, so that two roots reach the same value exactly when
    // they read the same text.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mix = |hash: &mut u64, value: u64| {
        for byte in value.to_le_bytes() {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(PRIME);
        }
    };
    for byte in path.to_string_lossy().to_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    if let Some(key) = content.get(path) {
        mix(&mut hash, key.content_hash);
        mix(&mut hash, key.context_hash);
    }
    for child in children {
        mix(&mut hash, child);
    }

    hashes.insert(path.to_path_buf(), hash);
    hash
}

/// The tail of a path, which is what a person reads in a list of thousands.
fn short(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    match path.parent().and_then(|parent| parent.file_name()) {
        Some(parent) => format!("{}/{name}", parent.to_string_lossy()),
        None => name,
    }
}
