//! **Every finding the rules make on a real project, grouped by code** — the measurement a false-positive sweep
//! starts from.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test diagnostics_sweep_probe -- --ignored --nocapture
//! ```
//!
//! It prints, per code, how many findings there are and the first few of them with `file:line` and the message — enough
//! to read the code around each one and decide whether the rule was right. Syntax errors are counted and then
//! **ignored**: they are the parser's answer about a file being typed at, not a rule's claim about a program.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_SWEEP_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named
        .join("CodeFormatCore")
        .join("src")
        .is_dir()
        .then_some(named)
}

/// Every C or C++ file under `root` the project would read — never the build output, which is generated copies.
fn sources_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == "out" || name == ".git" || name == ".cppls" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if matches!(
                extension.as_str(),
                "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hxx"
            ) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_the_rules_say_about_a_real_project() {
    let Some(root) = project() else {
        eprintln!("[sweep] the project is not here, nothing measured");
        return;
    };
    let mut files = sources_under(&root);
    // A cap that the caller can lift: the sweep is meant to be re-run on a bigger slice when a rule needs it.
    let cap: usize = std::env::var("CPPLS_SWEEP_FILES")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(usize::MAX);
    files.truncate(cap);

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.clone());
    for file in &files {
        session.want_cooked_reading(file);
    }
    for _ in 0..40 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

    type Found = (PathBuf, usize, String);
    let mut by_code: HashMap<String, Vec<Found>> = HashMap::new();
    let mut parse_errors = 0usize;
    let mut diagnosed = 0usize;
    for file in &files {
        let Some(found) = session.diagnostics(file) else {
            continue;
        };
        diagnosed += 1;
        parse_errors += found.errors.len();
        for check in &found.checks {
            let line = std::fs::read_to_string(file)
                .map(|text| {
                    text[..check.range.start_offset.min(text.len())]
                        .matches('\n')
                        .count()
                        + 1
                })
                .unwrap_or(0);
            by_code
                .entry(check.code.name().to_string())
                .or_default()
                .push((file.clone(), line, check.message.clone()));
        }
    }

    let mut codes: Vec<(String, Vec<Found>)> = by_code.into_iter().collect();
    codes.sort_by_key(|(_, findings)| std::cmp::Reverse(findings.len()));
    eprintln!(
        "[sweep] {} file(s) diagnosed, {parse_errors} parse error(s) (ignored), {} rule finding(s)",
        diagnosed,
        codes
            .iter()
            .map(|(_, findings)| findings.len())
            .sum::<usize>()
    );
    for (code, findings) in &codes {
        eprintln!("[sweep] {code:>24}: {}", findings.len());
        for (file, line, message) in findings.iter().take(6) {
            eprintln!(
                "[sweep]     {}:{line}  {message}",
                file.strip_prefix(&root).unwrap_or(file).display()
            );
        }
    }
}
