//! **Does indexing the project cost more than the files in it?** — the scaling measurement a design needs.
//!
//! ```text
//! $env:CPPLS_SCALE_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test scale_probe -- --ignored --nocapture
//! ```
//!
//! One session per size, from a cold store, adding the first `N` project files and timing `index_everything()`.
//! A cost linear in the files doubles when `N` doubles; a **second order** relation more than doubles, and the ratio
//! is what this prints.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::cache::SummaryKey;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project_files(root: &Path) -> Vec<PathBuf> {
    let compiler = root.join("build-cppls").join("compile_commands.json");
    let Ok(text) = std::fs::read_to_string(&compiler) else {
        return Vec::new();
    };
    // The `"file"` field of each entry, without a JSON dependency: the documents are one per line in this
    // generator's output, and a path never contains a quote.
    let mut files: Vec<PathBuf> = text
        .lines()
        .filter_map(|line| {
            let at = line.find("\"file\"")?;
            let rest = &line[at + 6..];
            let open = rest.find('"')?;
            let rest = &rest[open + 1..];
            let close = rest.find('"')?;
            Some(PathBuf::from(rest[..close].replace('/', "\\")))
        })
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// A cold store, **only when the probe is asked for one** — `CPPLS_SCALE_COLD=1`.
///
/// A probe that deleted a real checkout's cache by default would be destroying a colleague's warm start to take a
/// measurement, so the removal is opt-in and the default is "measure the session as it is".
fn remove_the_cache(root: &Path) {
    if std::env::var_os("CPPLS_SCALE_COLD").is_none() {
        return;
    }
    for name in [".cppls", ".cache"] {
        let _ = std::fs::remove_dir_all(root.join(name));
    }
    let _ = SummaryKey::new(0, 0);
}

#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn indexing_scales_how() {
    let root = std::env::var_os("CPPLS_SCALE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    if !root.is_dir() {
        eprintln!("skipped: no project");
        return;
    }
    let files = project_files(&root);
    if files.is_empty() {
        eprintln!("skipped: no compile_commands.json");
        return;
    }
    println!("{} project file(s)", files.len());

    let mut previous: Option<(usize, f64)> = None;
    for size in [1usize, 2, 4, 8, 16] {
        if size > files.len() {
            break;
        }
        remove_the_cache(&root);
        let mut session = Session::open(
            &root,
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&root),
        );
        let subset = files[..size].to_vec();
        session.add_project_files(subset.clone());

        let before = session.stats();
        let started = Instant::now();
        session.index_everything();
        let elapsed = started.elapsed().as_secs_f64();
        let change = session.stats().since(before);
        let per_file = elapsed / size as f64;

        let growth = previous
            .map(|(last_size, last_time)| {
                format!(
                    "  ×{:.2} for ×{:.0} files",
                    elapsed / last_time,
                    size as f64 / last_size as f64
                )
            })
            .unwrap_or_default();
        println!(
            "N {:>2}: {:>7.2} s  ({:>6.0} ms/file)  parsed {:>4} reused {:>4}{growth}",
            size,
            elapsed,
            per_file * 1000.0,
            change.rebuilt,
            change.reused,
        );
        previous = Some((size, elapsed));
    }

    remove_the_cache(&root);
}
