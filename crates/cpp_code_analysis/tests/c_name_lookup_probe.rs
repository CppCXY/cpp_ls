//! **Why a name that a header declares reads as undeclared** — the probe for `undefined-name` on a real project.
//!
//! ```text
//! $env:CPPLS_C_PROJECT = "E:\corpora\libuv"
//! cargo test --release -p cpp_code_analysis --test c_name_lookup_probe -- --ignored --nocapture
//! ```
//!
//! The finding this exists for, from libuv's Windows build: `src/win/core.c:847` says
//! `` `uv_translate_sys_error` is not declared: nothing this file can see names it `` — and the name is declared at
//! file scope in `include/uv.h:452`, which `core.c` includes on its ninth line. The check's own answer comes from
//! `ProjectIndex::definitions`, so the question is that call: does the index lack the declaration, or does the
//! *visibility* filter say this file cannot see the header?
//!
//! Both are printed, for the header and for every file that includes it, so the repair is a file rather than a guess.

use std::path::{Path, PathBuf};

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

fn project() -> Option<PathBuf> {
    let root = std::env::var_os("CPPLS_C_PROJECT").map(PathBuf::from)?;
    root.join("compile_commands.json").is_file().then_some(root)
}

fn name() -> String {
    std::env::var("CPPLS_PROBE_NAME").unwrap_or_else(|_| "uv_translate_sys_error".to_string())
}

fn file_named(root: &Path, needle: &str) -> PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !name.starts_with('.') && name != "build" {
                    pending.push(path);
                }
                continue;
            }
            if path.to_string_lossy().replace('\\', "/").ends_with(needle) {
                return path;
            }
        }
    }
    panic!("no file ending in {needle}");
}

#[test]
#[ignore = "opens a real project; set CPPLS_C_PROJECT and run with --ignored"]
fn a_name_a_header_declares() {
    let Some(root) = project() else {
        eprintln!("no project with a compile_commands.json: set CPPLS_C_PROJECT");
        return;
    };
    let name = name();
    let header = std::env::var_os("CPPLS_PROBE_HEADER")
        .map(PathBuf::from)
        .unwrap_or_else(|| file_named(&root, "include/uv.h"));
    let user = std::env::var_os("CPPLS_PROBE_USER")
        .map(PathBuf::from)
        .unwrap_or_else(|| file_named(&root, "src/win/core.c"));

    let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::open(&root, providers, WatchFilter::new(&root));
    session.index_everything();

    println!(
        "[probe] index holds a summary for {}: {}",
        header.display(),
        session.is_indexed(&header)
    );
    if let Some(summary) = session.index().summary(&header) {
        println!(
            "[probe] that summary: {} declaration(s), {} include(s), {} macro(s)",
            summary.declarations.len(),
            summary.includes.len(),
            summary.macros.len()
        );
        let names: Vec<&str> = summary
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        println!(
            "[probe] first 12 names: {:?}",
            &names[..names.len().min(12)]
        );
        // **Where the reading stops**, which is what a missing name is usually about: the last facts and the line
        // each was written on. A header whose summary holds a prefix of its declarations has a construct the walk
        // gave up at, and that construct is the last fact's own line rather than the missing name's.
        if let Ok(text) = std::fs::read_to_string(&header) {
            for fact in summary.declarations.iter().rev().take(6) {
                let at = fact.name_range.start_offset.min(text.len());
                let line = text[..at].matches('\n').count() + 1;
                println!("[probe] late fact {:?} at line {line}", fact.name.as_str());
            }
        }
        for wanted in [
            "uv_translate_sys_error",
            "uv_strerror",
            "uv_loop_init",
            "uv_loop_t",
        ] {
            println!(
                "[probe] does the summary name {wanted}: {}",
                names.contains(&wanted)
            );
        }
    }
    println!(
        "[probe] {} can see {}: {:?}",
        user.display(),
        name,
        session.index().definitions(&name, &user)
    );
    println!(
        "[probe] {} (from the header's own file): {:?}",
        name,
        session.index().definitions(&name, &header)
    );
    // **The program read**, which is what a server does for an open file — and the path that cooks the whole
    // closure. Asked here because the summary a program read commits and the one an include cascade commits are
    // built from different text (a unit stream versus the file's own text), and only one of them was wrong.
    let core_text = std::fs::read_to_string(&user).unwrap_or_default();
    session.did_open(&user, &core_text);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    println!(
        "[probe] a program is owed now: {:?}",
        session.the_program_to_read()
    );
    let reading = session.read_a_program_now();
    println!(
        "[probe] program read: {:?}",
        reading
            .as_ref()
            .map(|reading| (reading.files, reading.tokens, reading.errors))
    );
    if let Some(summary) = session.index().summary(&header) {
        println!(
            "[probe] after the program read, that summary: {} declaration(s)",
            summary.declarations.len()
        );
    }
    println!(
        "[probe] after the program read, {} can see {}: {:?}",
        user.display(),
        name,
        session.index().definitions(&name, &user)
    );
    println!(
        "[probe] who includes src/win/internal.h: {:?}",
        session
            .index()
            .dependents_of(&root.join("src/win/internal.h"))
    );
}
