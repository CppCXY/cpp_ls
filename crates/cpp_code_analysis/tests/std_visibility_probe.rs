//! **Why an interface unit sees less of `std` than a `.cpp` does** — a reader's report, measured.
//!
//! The report: in `report.cppm`, `std::str` completed `stringbuf`, `stringstream`, `strong_ordering` — and not
//! `string`. This probe asks the index and the visibility question separately, because the two answers together say
//! where the defect is.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test std_visibility_probe -- --ignored --nocapture
//! ```
//!
//! # What it measured, on the reader's project
//!
//! ```text
//! the index of `std`, from main.cpp        4898 declarations, string=["Type"], string_view=["Type"]
//! the index of `std`, from report.cppm     4110 declarations, string=[],        string_view=[]
//! visible files from main.cpp              173
//! visible files from report.cppm           151   ← the 22 missing include `xstring`
//! ```
//!
//! **The index is right; the visible set is wrong.** `xstring` — where `std::string` is declared — is reached from
//! `main.cpp` because that file `#include`s `"extra/widget.h"`, which pulls the standard headers in textually. From
//! `report.cppm` it is reached only through `import std;`, and the module frontier walks the interface's `#include`
//! edges with a macro state that is not the unit's, so a header the unit included behind `_HAS_CXX20` is dropped. The
//! unit's *rendering* has already decided those guards — its `walked` list is 223 files — so the fix is to let that
//! list speak for the module.
//!
//! # What was tried and is not landed
//!
//! Recording the closure (`ProjectIndex::unit_closures`, filled from `RenderedUnit::walked`), adding its files to the
//! visible set in `walk_the_module_edges`, and clearing the visibility memo when it is recorded: the closure **was**
//! recorded (223 files, confirmed through a counter on the index) and the walk still answered 151. So the closure
//! reaches the index but not the walk that answers `visible_files` — the next step is finding which of the two
//! visibility implementations answers that call and why the added files do not survive to the count. Both attempts
//! were reverted rather than left in.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_MODULES_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp_modules"));
    named
        .join("src")
        .join("report.cppm")
        .is_file()
        .then_some(named)
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn kind_name(kind: cpp_code_analysis::DeclKind) -> &'static str {
    match kind {
        cpp_code_analysis::DeclKind::Type => "Type",
        cpp_code_analysis::DeclKind::Function => "Function",
        cpp_code_analysis::DeclKind::Variable => "Variable",
        cpp_code_analysis::DeclKind::Namespace => "Namespace",
        _ => "Other",
    }
}

#[test]
#[ignore = "a probe: needs the external fixture"]
fn what_the_index_of_std_holds_and_who_can_see_it() {
    let Some(root) = project() else {
        eprintln!("no fixture: set CPPLS_MODULES_PROJECT");
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let interface = root.join("src").join("report.cppm");
    let main = root.join("src").join("main.cpp");
    let text = std::fs::read_to_string(&interface).expect("report.cppm");
    session.did_open(&interface, &text);
    session.add_project_files([interface.clone(), main.clone()]);
    session.index_everything();
    println!(
        "module read: {:?}",
        session.read_the_modules_a_file_imports(&interface)
    );

    // **The index, by name**, asked from both files: a `Type` there and absent here is a visibility answer.
    for (label, file) in [("main.cpp", &main), ("report.cppm", &interface)] {
        if session.index().summary(file).is_none() {
            continue;
        }
        let holders = session.index().declarations_in("std", file);
        let mut found: Vec<String> = Vec::new();
        for wanted in [
            "string",
            "string_view",
            "size_t",
            "vector",
            "format",
            "stringbuf",
            "strong_ordering",
        ] {
            let kinds: Vec<&str> = holders
                .iter()
                .filter(|declaration| declaration.fact.name.as_str() == wanted)
                .map(|declaration| kind_name(declaration.fact.kind))
                .collect();
            found.push(format!("{wanted}={kinds:?}"));
        }
        println!(
            "  {label}: {} visible file(s), {} in `std`: {}",
            session.index().visible_files(file).len(),
            holders.len(),
            found.join(" ")
        );
    }

    // **The diff, by file name** — what the interface unit cannot see that `main.cpp` can.
    let interface_files: std::collections::BTreeSet<String> = session
        .index()
        .visible_files(&interface)
        .into_iter()
        .map(|(path, _)| name_of(&path))
        .collect();
    let main_files: std::collections::BTreeSet<String> = session
        .index()
        .visible_files(&main)
        .into_iter()
        .map(|(path, _)| name_of(&path))
        .collect();
    let only_main: Vec<&String> = main_files.difference(&interface_files).collect();
    println!(
        "  in main and not in the interface ({}): {only_main:?}",
        only_main.len()
    );

    // **And the facts the definition leaves behind**, which is what Go to Implementation scans for.
    let implementation = root.join("src").join("report.cpp");
    for path in [&interface, &implementation] {
        let Some(summary) = session.index().summary(path) else {
            continue;
        };
        let facts: Vec<String> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name.as_str() == "joined" || fact.name.as_str() == "add")
            .map(|fact| format!("{} scope {:?}", fact.name, fact.scope))
            .collect();
        println!(
            "  {}: {facts:?}",
            path.file_name().unwrap().to_string_lossy()
        );
    }

    // **How the list narrows as the reader types** — the same query at each prefix length, with the truncation flag
    // the client uses to decide whether to ask again.
    for typed in [
        "std::",
        "std::s",
        "std::st",
        "std::str",
        "std::string",
        "std::string_v",
    ] {
        let probe = format!("{text}\nvoid probe_scope() {{\n    {typed}\n}}\n");
        session.did_open(&interface, &probe);
        session.index_everything();
        let view = session.view_of_the_file(&interface).expect("a view");
        let offset = probe.rfind(typed).expect("the prefix") + typed.len();
        let started = std::time::Instant::now();
        let offered = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(offset),
        );
        println!(
            "  `{typed}`: {} item(s), truncated {}, {:?}",
            offered.items.len(),
            offered.truncated,
            started.elapsed()
        );
    }
    // **Are the intrinsics really filed under `std`?** The reader's `std::` list began with `_mm256_*`, which are not
    // `std` names in any standard.
    {
        let holders = session.index().declarations_in("std", &interface);
        let intrinsics: Vec<&str> = holders
            .iter()
            .filter(|declaration| declaration.fact.name.starts_with("_mm"))
            .map(|declaration| declaration.fact.name.as_str())
            .take(10)
            .collect();
        println!(
            "  `_mm*` in the index of `std`: {} total, first {:?}",
            holders
                .iter()
                .filter(|declaration| declaration.fact.name.starts_with("_mm"))
                .count(),
            intrinsics
        );
    }


    // **Which file says they are in `std`?**
    for (file, fact) in session.index().every_fact_named("_mm_abs_epi16") {
        println!(
            "  `_mm_abs_epi16`: {} scope {:?} kind {:?}",
            file.display(),
            fact.scope,
            fact.kind
        );
    }

    // **The first items of the empty-prefix `std::` list** — the report is that they are `_mm256_*` intrinsics, which
    // are not `std` names at all.
    for typed in ["std::", "std::s"] {
        let probe = format!("{text}\\nvoid probe_scope() {{\\n    {typed}\\n}}\\n");
        session.did_open(&interface, &probe);
        session.index_everything();
        let view = session.view_of_the_file(&interface).expect("a view");
        let offset = probe.rfind(typed).expect("the prefix") + typed.len();
        let offered = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(offset),
        );
        let labels: Vec<String> = offered
            .items
            .iter()
            .take(20)
            .map(|item| item.label.clone())
            .collect();
        println!(
            "  `{typed}` first 20 (scope {:?}, prefix {:?}): {labels:?}",
            offered.scope, offered.prefix
        );
    }

    // **And the completion the reader saw**, for the record.
    let typed = "std::str";
    let probe = format!("{text}\nvoid probe_scope() {{\n    {typed}\n}}\n");
    session.did_open(&interface, &probe);
    session.index_everything();
    let view = session.view_of_the_file(&interface).expect("a view");
    let offset = probe.rfind(typed).expect("the prefix") + typed.len();
    let offered = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    );
    let labels: Vec<String> = offered
        .items
        .iter()
        .map(|item| item.label.clone())
        .collect();
    println!(
        "completion after `{typed}` in report.cppm: {} item(s) [{}]",
        labels.len(),
        &labels[..labels.len().min(14)].join(", ")
    );
}
