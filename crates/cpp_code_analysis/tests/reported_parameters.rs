//! **A reader's two reports, checked against their own project** — the fixture at `CPPLS_MODULES_PROJECT`.
//!
//! Both were about a body inside a namespace: `entry` in `Archive::put` and `line` in `Report::add` "are not declared",
//! and hovering `line` answered `std::source_location::line`. The cause was one rule: a qualified declaration nested in
//! a `CompoundStat` was skipped, and a namespace body is a `CompoundStat`.
//!
//! Skipped when the fixture is absent, like every other probe.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_MODULES_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp_modules"));
    named
        .join("src")
        .join("archive.cpp")
        .is_file()
        .then_some(named)
}

#[test]
#[ignore = "a probe: needs the external fixture"]
fn a_body_inside_a_namespace_names_its_parameters() {
    let Some(root) = project() else {
        eprintln!("no fixture: set CPPLS_MODULES_PROJECT");
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let archive = root.join("src").join("archive.cpp");
    let report = root.join("src").join("report.cpp");
    let archive_text = std::fs::read_to_string(&archive).expect("archive.cpp");
    let report_text = std::fs::read_to_string(&report).expect("report.cpp");
    session.did_open(&archive, &archive_text);
    session.did_open(&report, &report_text);
    session.add_project_files([archive.clone(), report.clone()]);
    session.index_everything();

    for (path, text, needle) in [
        (&archive, &archive_text, "_counts[entry.key]"),
        (&report, &report_text, "std::move(line)"),
    ] {
        let at = text.find(needle).expect("the reported line");
        let name = if needle.contains("entry") {
            at + "_counts[".len() + 1
        } else {
            at + "std::move(".len() + 1
        };
        let view = session.view_of_the_file(path).expect("a view");
        let found = session.definition(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(name),
        );
        let rendered = match &found {
            Known::Yes(definition) => format!(
                "{} in {:?}",
                definition.fact.name,
                definition.file.file_name()
            ),
            other => format!("{other:?}"),
        };
        println!(
            "{}: {needle:?} → {rendered}",
            path.file_name().unwrap().to_string_lossy()
        );
    }

    for path in [&archive, &report] {
        if let Some(diagnostics) = session.diagnostics(path) {
            let messages: Vec<String> = diagnostics
                .checks
                .iter()
                .map(|entry| format!("{:?}: {}", entry.code, entry.message))
                .collect();
            let errors: usize = diagnostics.errors.len();
            println!(
                "{}: {} error(s), {} check(s) {}",
                path.file_name().unwrap().to_string_lossy(),
                errors,
                messages.len(),
                messages.join(" | ")
            );
        }
    }

    // **The answer**: the parameters are declared, so no entry says otherwise.
    if let Some(diagnostics) = session.diagnostics(&archive) {
        assert!(
            !diagnostics
                .errors
                .iter()
                .any(|error| error.message.contains("entry"))
                && !diagnostics
                    .checks
                    .iter()
                    .any(|check| check.message.contains("entry")),
            "`entry` is the parameter of the definition it is written in: {:?}",
            diagnostics
                .errors
                .iter()
                .map(|error| error.message.clone())
                .collect::<Vec<_>>()
        );
    }
    if let Some(diagnostics) = session.diagnostics(&report) {
        assert!(
            !diagnostics.checks.iter().any(|check| {
                check.message.contains("push_back") || check.message.contains("_Ty")
            }),
            "`push_back(const _Ty&)` takes a moved string: {:?}",
            diagnostics
                .checks
                .iter()
                .map(|check| check.message.clone())
                .collect::<Vec<_>>()
        );
    }
}
