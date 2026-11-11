//! **The real project, through the session the server uses** — not through `inspect`.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test real_project_probe -- --ignored --nocapture
//! ```
//!
//! `inspect` answers one file's own reading and does **not** inline its includes (measured: the "cooked" tree of
//! `LuaCodeFormat.cpp` was 10 086 bytes, the file's own size), so it cannot reproduce anything that depends on a header.
//! This opens the project the way the server does — every file, the closure pumped — and asks the two questions the
//! editor reports: the type of `result` at `LuaCodeFormat.cpp:304`, and how the qualifier of the definition at line 300
//! is classified.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named
        .join("CodeFormatLib")
        .join("src")
        .is_dir()
        .then_some(named)
}

fn sources_under(root: &std::path::Path) -> Vec<PathBuf> {
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
            if name == "out" || name == ".git" || name == ".cppls" || name == "3rd" {
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
                "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx"
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
fn what_the_session_says_about_the_reported_line() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    let file = root.join("CodeFormatLib/src/LuaCodeFormat.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[probe] no LuaCodeFormat.cpp");
        return;
    };

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files = sources_under(&root);
    session.add_project_files(files.iter().cloned());
    session.want_cooked_reading(&file);
    // **`catch_up`, not only `index_everything`.** The includes a file names are read by `catch_up` — that is the
    // request the server makes when a file is opened — and without it a header that is merely *included* stays
    // unread no matter how many times the queue is pumped.
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        // **The refinement pass is the one that reads a file with its closure's macros in force** — the initial pass
        // builds a plain summary, so `_STD_BEGIN` is undefined there and the declarations land at global scope.
        session.want_refinement(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    let Some(view) = session.view(&file) else {
        eprintln!("[probe] no cooked reading");
        return;
    };

    // **Is the standard library in the rendering at all?** The renderings are big, so the questions are asked of the
    // whole text rather than per node.
    let rendered = view.root.text().to_string();
    eprintln!(
        "[probe] the rendering is {} byte(s); `class vector` {}; `emplace_back` {}",
        rendered.len(),
        if rendered.contains("class vector") {
            "present"
        } else {
            "absent"
        },
        if rendered.contains("emplace_back") {
            "present"
        } else {
            "absent"
        },
    );
    // **Is the closure read at all?** The standard header's own summary is the question: if it is absent, the session
    // never read the include; if it is present, the visibility answer is what fails.
    let msvc = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.51.36231\include\vector",
    );
    eprintln!(
        "[probe] summaries: {}; the MSVC `vector` summary: {}",
        session.index().summaries().count(),
        if session.index().summary(&msvc).is_some() {
            "present"
        } else {
            "absent"
        }
    );
    let style = root.join("CodeFormatCore/include/CodeFormatCore/Config/LuaStyle.h");
    eprintln!(
        "[probe] LuaStyle.h summary: {}",
        if session.index().summary(&style).is_some() {
            "present"
        } else {
            "absent"
        }
    );
    for spelled in ["vector", "std::vector", "begin"] {
        match session.index().definitions(spelled, &file) {
            Known::Yes(found) => eprintln!(
                "[probe] `{spelled}`: {} declaration(s), first from {}",
                found.found.len(),
                found.found[0].file.display()
            ),
            Known::Unknown(reason) => eprintln!("[probe] `{spelled}`: unknown — {reason:?}"),
            Known::No => eprintln!("[probe] `{spelled}`: no"),
        }
    }
    // **Does the index know `std::vector` from this file?** A qualified name, asked of the same visibility the type
    // query uses.
    match session.index().definitions("std::vector", &file) {
        Known::Yes(found) => {
            for definition in &found.found {
                eprintln!(
                    "[probe] std::vector -> {} (kind {:?}, range {:?})",
                    definition.file.display(),
                    definition.fact.kind,
                    definition.fact.name_range
                );
            }
            eprintln!("[probe] std::vector: {} declaration(s)", found.found.len());
        }
        Known::Unknown(reason) => eprintln!("[probe] std::vector: unknown — {reason:?}"),
        Known::No => eprintln!("[probe] std::vector: no"),
    }
    // **`result` in `auto &result = results.emplace_back();`**
    let Some(at) = text.find("auto &result = results.emplace_back") else {
        eprintln!("[probe] the line is not there");
        return;
    };
    let cursor = at + "auto &".len();
    let reading = view
        .reading_offset_of(FileOffset::new(cursor))
        .unwrap_or(ReadingOffset::new(cursor));
    match session.type_at(&view, reading) {
        Known::Yes(found) => eprintln!("[probe] result: `{}`", found.type_of),
        Known::Unknown(reason) => eprintln!("[probe] result: unknown — {reason:?}"),
        Known::No => eprintln!("[probe] result: no"),
    }

    // **The qualifier of `std::vector<...> LuaCodeFormat::MakeDiagnosticInfo(…)`, line 300.**
    let Some(at) = text.find("LuaCodeFormat::MakeDiagnosticInfo") else {
        eprintln!("[probe] the definition is not there");
        return;
    };
    let names = session.classified_names(&view);
    for name in &names {
        if name.range.start_offset <= at && at < name.range.end_offset() {
            eprintln!(
                "[probe] the qualifier `LuaCodeFormat` at {at} is classified {:?} (declaration: {})",
                name.kind, name.declaration
            );
        }
    }
    let count = names
        .iter()
        .filter(|name| name.range.start_offset == at)
        .count();
    eprintln!("[probe] {} name(s) start exactly at the qualifier", count);
}
