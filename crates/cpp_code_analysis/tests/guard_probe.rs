//! **Why an `#include` behind `#ifdef HAVE_…` still warns** — the two neovim includes the branch rule did not silence.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test guard_probe -- --ignored --nocapture
//! ```
//!
//! The include-not-found check asks [`Session::index`]'s `the_guard_is_taken`, which is
//! [`crate::index::environment::visibility_at`]: the guard's condition is evaluated against `macros_at`, the macro
//! state at that offset. A `#ifdef HAVE_LANGINFO_H` is `Inactive` only when the state can say the name is *not*
//! defined, and a state that merely does not mention it answers `Unknown` — deliberately, because "no witness mentions
//! it" is a claim only a session that knows the compiler's own predefines may make.
//!
//! So this prints, for each of the two remaining findings: whether the compilation environment is **complete**
//! (`Marked::is_incomplete`), whether the name is mentioned, and the verdict.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The offset of the line above `at`, so the question is asked before the guarded directive is reached.
fn text_before(path: &std::path::Path, at: usize) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let end = at.min(text.len());
    text[..end].rfind('\n').map_or(0, |line| line + 1)
}

fn project() -> PathBuf {
    std::env::var_os("CPPLS_NEOVIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\neovim_test\neovim"))
}

#[test]
#[ignore = "a probe: needs the neovim checkout"]
fn why_the_two_platform_includes_still_warn() {
    let root = project();
    let files = ["src/nvim/log.c", "src/nvim/mbyte.c"];
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.iter().map(|file| root.join(file)));
    for _ in 0..8 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

    let environment = session.index().macros();
    eprintln!(
        "[guard] the compilation environment is {}",
        if environment.is_incomplete() {
            "INCOMPLETE (a name nobody mentions is Unknown)"
        } else {
            "complete (a name nobody mentions is not defined)"
        }
    );

    for (file, spelling, name) in [
        ("src/nvim/log.c", "execinfo.h", "HAVE_EXECINFO_BACKTRACE"),
        ("src/nvim/mbyte.c", "langinfo.h", "HAVE_LANGINFO_H"),
    ] {
        let path = root.join(file);
        let Some(summary) = session.index().summary(&path) else {
            eprintln!("[guard] {file}: no summary — the file was not read");
            continue;
        };
        let Some(include) = summary.includes.iter().find(|it| it.spelling == spelling) else {
            eprintln!("[guard] {file}: no include of {spelling}");
            continue;
        };
        let at = include.range.start_offset;
        let taken = session.index().the_guard_is_taken(&path, include.guard, at);
        let here = session.index().macros_at(&path, at);
        // **Is the doubt the walk's own?** The base environment is complete; if the state at this offset is
        // incomplete, something the walk passed above it could define anything — and the include being guarded
        // is at the next line, so a walk that resolved *it* early would be doubting the name on the strength of
        // the very directive it decides.
        let before = text_before(&path, at);
        let earlier = session.index().macros_at(&path, before);
        eprintln!(
            "[guard]   walk state: complete_before={} incomplete_here={} base_complete={}",
            !earlier.is_incomplete(),
            here.is_incomplete(),
            !environment.is_incomplete()
        );
        eprintln!(
            "[guard] {file}: {spelling} guard {:?} (region {}) -> taken={taken}; {name}: unknown={} mentioned={}",
            include.guard,
            match include.guard {
                cpp_code_analysis::FactGuard::Region(region) => region as usize,
                cpp_code_analysis::FactGuard::Unconditional => usize::MAX,
            },
            here.is_unknown(name),
            here.names_the_compilation(name),
        );
        // The condition's own text, so the report says which spelling was asked about.
        if let cpp_code_analysis::FactGuard::Region(region) = include.guard
            && let Some(range) = summary.guards.regions.get(region as usize)
            && let Some(text) = std::fs::read_to_string(&path).ok()
            && let Some(condition) = text.get(range.start_offset..range.end_offset())
        {
            eprintln!("[guard]   condition: {}", condition.trim());
        }
    }
}

/// **Which include in the closure takes the completeness claim back?**
///
/// `mark_incomplete` fires on an `#include` the walk reaches and cannot resolve, and from then on every name nothing
/// mentions is `Unknown` — which is what leaves `#ifdef HAVE_EXECINFO_BACKTRACE` undecided in `log.c`. So the
/// question is which include that is, and whether it is genuinely absent on this machine or a search defect.
///
/// The walk here is the closure's: log.c, its includes, theirs. A file whose summary the session has not read is
/// added and indexed, so the answer covers the whole closure rather than the first hop.
#[test]
#[ignore = "a probe: needs the neovim checkout"]
fn which_include_in_the_closure_does_not_resolve() {
    let root = project();
    let start = root.join("src/nvim/log.c");
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([start.clone()]);
    for _ in 0..8 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

    let mut pending = vec![start.clone()];
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut missing: Vec<(PathBuf, String)> = Vec::new();
    let mut read = 0usize;
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let summary = match session.index().summary(&path) {
            Some(summary) => summary.clone(),
            None => {
                session.add_project_files([path.clone()]);
                for _ in 0..4 {
                    session.index_everything();
                    if session.index().summary(&path).is_some() || session.pending_work() == 0 {
                        break;
                    }
                }
                read += 1;
                match session.index().summary(&path) {
                    Some(summary) => summary.clone(),
                    None => continue,
                }
            }
        };
        for include in &summary.includes {
            match &include.resolved {
                Some(target) => pending.push(target.clone()),
                None => missing.push((path.clone(), include.spelling.clone())),
            }
        }
    }

    eprintln!(
        "[closure] {read} file(s) read on demand, {} unresolved include(s) in {} file(s)",
        missing.len(),
        seen.len()
    );
    missing.sort();
    missing.dedup();
    for (file, spelling) in missing.iter().take(40) {
        eprintln!(
            "[closure]   {} -> {spelling}",
            file.strip_prefix(&root).unwrap_or(file).display()
        );
    }
}
