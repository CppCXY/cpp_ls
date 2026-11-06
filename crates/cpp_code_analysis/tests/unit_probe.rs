//! **Where `import std;`'s 152 s goes** — a probe that splits the program read into its stages.
//!
//! ```text
//! $env:CPPLS_MODULES_PROJECT = "C:\Users\zc\Desktop\cpp_modules"
//! cargo test --release -p cpp_code_analysis --test unit_probe -- --ignored --nocapture
//! ```
//!
//! `read_the_unit` is `prepare_a_unit_reading` (render the closure, parse it, sweep the declarations — no `&mut`, so
//! it can be paid outside the write lock) followed by `commit_a_unit_reading` (one `insert_cooked` per file). The two
//! are measured apart because they have different caches — a rendering by closure key, the facts by program hash — and
//! "the module read is slow" has to say *which* of them is missing before anything can be fixed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_MODULES_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp_modules"));
    named.is_dir().then_some(named)
}

/// The standard module's interface unit on this machine — the file `import std;` resolves to.
fn the_std_unit() -> Option<PathBuf> {
    let msvc = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215",
    )
    .join("modules")
    .join("std.ixx");
    if msvc.is_file() {
        return Some(msvc);
    }
    let gcc = PathBuf::from(r"C:\Users\zc\Desktop\mingw64\lib\gcc\x86_64-w64-mingw32\15.1.0")
        .join("include")
        .join("c++")
        .join("bits")
        .join("std.cc");
    gcc.is_file().then_some(gcc)
}

fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
                if name != "build" && name != "out" && !name.starts_with('.') {
                    pending.push(path);
                }
                continue;
            }
            if matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("cpp" | "cppm" | "cc" | "cxx" | "ixx")
            ) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn where_the_module_read_spends_its_time() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_MODULES_PROJECT)");
        return;
    };
    let Some(unit) = the_std_unit() else {
        eprintln!("skipped: no standard module on this machine");
        return;
    };
    let files = sources(&root);

    let open = |root: &Path, files: &[PathBuf]| {
        let mut session = Session::open(
            root.to_path_buf(),
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(root),
        );
        session.add_project_files(files.to_vec());
        session.index_everything();
        session
    };

    // **One session, two reads**: the first pays whatever is not cached, the second answers from the session's memo.
    let mut session = open(&root, &files);
    session.add_project_files([unit.clone()]);
    // The unit must be in the index before its program can be read: `gather_a_unit_read` starts from its summary.
    session.index_everything();

    for round in ["first", "second"] {
        let started = Instant::now();
        let Some(prepared) = session.prepare_a_unit_reading(&unit) else {
            println!("{round}: the unit cannot be prepared");
            return;
        };
        let prepare = started.elapsed();
        let started = Instant::now();
        let reading = session.commit_a_unit_reading(prepared);
        let commit = started.elapsed();
        println!(
            "{round:>6}: prepare {:>8.2} s, commit {:>8.2} s — {} file(s), {} declaration(s), {} token(s), {} error(s)",
            prepare.as_secs_f64(),
            commit.as_secs_f64(),
            reading.files,
            reading.declared,
            reading.tokens,
            reading.errors,
        );
        println!(
            "        files_with_tokens {}, missing {}, unplaced {}, braces {}, unbalanced {:?}",
            reading.files_with_tokens,
            reading.missing,
            reading.unplaced,
            reading.braces,
            &reading.unbalanced[..reading.unbalanced.len().min(4)],
        );
    }

    // **A fresh session over the same cache directory**: this is the restart a user sees, and it is the one the
    // stored unit and the stored cooked readings are supposed to serve.
    drop(session);
    let mut restarted = open(&root, &files);
    restarted.add_project_files([unit.clone()]);
    restarted.index_everything();
    let started = Instant::now();
    let Some(prepared) = restarted.prepare_a_unit_reading(&unit) else {
        println!("restart: the unit cannot be prepared");
        return;
    };
    let prepare = started.elapsed();
    let started = Instant::now();
    let reading = restarted.commit_a_unit_reading(prepared);
    let commit = started.elapsed();
    println!(
        "restart: prepare {:>8.2} s, commit {:>8.2} s — {} file(s), {} declaration(s), {} token(s)",
        prepare.as_secs_f64(),
        commit.as_secs_f64(),
        reading.files,
        reading.declared,
        reading.tokens,
    );
}
