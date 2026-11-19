//! **The two hover reports from `CodeFormatServer/src/LSP/LSP.cpp`, asked in rendering coordinates.**
//!
//! The file→rendering mapping is a layer of its own; this probe skips it by finding the token **in the rendering** and
//! asking there, so the answer is the analysis's and not the mapping's.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test -p cpp_ls --test lsp_hover_probe -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

/// Every source the server and the core of the project have, so a hover's closure is really indexed.
fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![
        root.join("CodeFormatServer/src"),
        root.join("CodeFormatCore/src"),
    ];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry = entry.path();
            if entry.is_dir() {
                pending.push(entry);
            } else if entry
                .extension()
                .is_some_and(|it| it == "cpp" || it == "h" || it == "hpp")
            {
                files.push(entry);
            }
        }
    }
    files
}

fn card(session: &Session<DiskFiles>, path: &Path, at: usize) -> String {
    let view = session.view_of_the_file(path).expect("the file has a view");
    let reading = ReadingOffset::new(at);
    let file = view
        .file_offset_of(reading)
        .unwrap_or_else(|| FileOffset::new(at));
    match cpp_ls::handlers::hover::hover(session, &view, reading, file) {
        None => "NO CARD".to_string(),
        Some(hover) => match hover.contents {
            lsp_types::HoverContents::Markup(markup) => markup.value,
            other => format!("{other:?}"),
        },
    }
}

#[test]
#[ignore = "a probe: opens a real project and prints the two hover cards"]
fn the_two_reported_hovers() {
    let root = project();
    let path = root.join("CodeFormatServer/src/LSP/LSP.cpp");
    let text = std::fs::read_to_string(&path).expect("the reported file is here");
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files = project_files(&root);
    println!("{} project file(s)", files.len());
    session.add_project_files(files);
    session.did_open(&path, &text);
    let started = std::time::Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 4000 {
        session.advance(64);
        slices += 1;
        if slices.is_multiple_of(400) {
            let (pending, open, rest) = session.queue_sizes();
            println!(
                "[pump] {slices}: queue {pending} ({open} open / {rest} rest), cooking {}, refinements {}, re-read {}",
                session.pending_cooking(),
                session.refinements_wanted(),
                session.re_read_backlog()
            );
        }
    }
    let _ = session.cook(&path);
    // **The demand the query makes**: one ask for the class's own file, then the pump.
    let json_file = root.join("3rd/nlohmann_json/include/nlohmann/json.hpp");
    session.want_refinement(&json_file);
    let asked = session.refinements_wanted();
    let mut demand_slices = 0usize;
    while session.pending_work() > 0 && demand_slices < 2000 {
        session.advance(64);
        demand_slices += 1;
        if demand_slices.is_multiple_of(500) {
            println!(
                "[demand] {demand_slices}: queue {}, cooking {}, refinements {}, re-read {}",
                session.pending(),
                session.pending_cooking(),
                session.refinements_wanted(),
                session.re_read_backlog()
            );
        }
    }
    println!(
        "[demand] asked {asked}, drained in {demand_slices} slice(s), leftover {}",
        session.pending_work()
    );
    // **The pump's own closure walk**, with LSP.cpp as the open document — the state an editor is in.
    session.want_the_closure_cooked(&path);
    let mut closure_slices = 0usize;
    while session.pending_work() > 0 && closure_slices < 4000 {
        session.advance(64);
        closure_slices += 1;
    }
    println!(
        "[pump] closure walk: {closure_slices} slice(s), pending {}",
        session.pending_work()
    );
    println!(
        "indexed in {:?} ({slices} slice(s), pending {})",
        started.elapsed(),
        session.pending_work()
    );
    let view = session
        .view_of_the_file(&path)
        .expect("the file has a view");
    let rendering = view.source.to_string();

    // **How much of nlohmann's class body the index captured** — the member facts are the ones a qualified
    // lookup for `json::object` needs, and the answer was `NotDeclaredHere`.
    {
        let json = root.join("3rd/nlohmann_json/include/nlohmann/json.hpp");
        if let Some(summary) = session.index().summary(&json) {
            let mut scopes: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for declaration in &summary.declarations {
                let scope = declaration
                    .scope
                    .as_ref()
                    .map_or("(none)".to_string(), |s| s.to_string());
                *scopes.entry(scope).or_default() += 1;
            }
            let top: Vec<(String, usize)> = scopes
                .iter()
                .filter(|(scope, _)| scope.contains("basic_json"))
                .map(|(scope, count)| (scope.clone(), *count))
                .take(12)
                .collect();
            let names: Vec<String> = summary
                .declarations
                .iter()
                .filter(|declaration| {
                    declaration
                        .scope
                        .as_ref()
                        .is_some_and(|scope| scope.as_str() == "nlohmann::basic_json")
                })
                .map(|declaration| declaration.name.to_string())
                .collect();
            println!(
                "[summary] json.hpp: {} declaration(s), basic_json scopes {top:?}",
                summary.declarations.len()
            );
            println!("[summary] members: {}", names.join(" "));
            let tail: Vec<String> = summary
                .declarations
                .iter()
                .rev()
                .take(12)
                .map(|declaration| {
                    format!(
                        "{}::{}",
                        declaration.scope.as_ref().map_or("(none)", |s| s.as_str()),
                        declaration.name
                    )
                })
                .collect();
            println!("[summary] last declarations: {}", tail.join(" "));
            if let Some(last) = summary
                .declarations
                .iter()
                .filter(|declaration| {
                    declaration
                        .scope
                        .as_ref()
                        .is_some_and(|scope| scope.as_str() == "nlohmann::basic_json")
                })
                .max_by_key(|declaration| declaration.name_range.start_offset)
            {
                let written = std::fs::read_to_string(&json).unwrap_or_default();
                let at = last.name_range.start_offset;
                let text = written.get(at.saturating_sub(120)..(at + 40).min(written.len()));
                println!(
                    "[summary] last scoped member {} at {at}: {:?}",
                    last.name,
                    text.unwrap_or("")
                );
            }
            for wanted in ["parse", "dump", "size", "object"] {
                let found: Vec<String> = summary
                    .declarations
                    .iter()
                    .filter(|declaration| declaration.name == wanted)
                    .map(|declaration| {
                        format!(
                            "{}::{} ({:?})",
                            declaration.scope.as_ref().map_or("(none)", |s| s.as_str()),
                            declaration.name,
                            declaration.kind
                        )
                    })
                    .collect();
                println!("[summary] {wanted}: {found:?}");
            }
        } else {
            println!("[summary] json.hpp: no summary");
        }
    }
    // **The two index questions the hover needs**, asked directly.
    {
        let in_scope = session
            .index()
            .declarations_in("nlohmann::basic_json", &path)
            .len();
        let by_name = session
            .index()
            .definition("nlohmann::basic_json::object", &path);
        println!(
            "[index] declarations_in(basic_json) = {in_scope}, definition(basic_json::object) = {by_name:?}"
        );
    }
    // **Is the class's header cooked at all**, and does its cooked reading hold the member?
    {
        let json = root.join("3rd/nlohmann_json/include/nlohmann/json.hpp");
        // **The contract**: a query that cannot answer asks for the class's file, the pump renders it, the next
        // query answers. The ask happens inside `Session::definition`; here the pump is run explicitly.
        let object_at = {
            let view = session
                .view_of_the_file(&path)
                .expect("the file has a view");
            view.source
                .find("json::object")
                .expect("the call is written")
                + "json::".len()
        };
        {
            let view = session
                .view_of_the_file(&path)
                .expect("the file has a view");
            let first = session.definition(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(object_at),
            );
            println!("[ask] first query: {first:?}");
        }
        let mut demand_slices = 0usize;
        while session.pending_work() > 0 && demand_slices < 400 {
            session.advance(64);
            demand_slices += 1;
        }
        println!(
            "[ask] pump after the query: {demand_slices} slice(s), pending {}",
            session.pending_work()
        );
        // **The two measurements the goal asks for**: what a cook costs, and what the rendered class body holds.
        {
            let cook_started = std::time::Instant::now();
            let again = session.cook(&json);
            println!(
                "[measure] cook(json.hpp) = {:?}, a reading: {}",
                cook_started.elapsed(),
                again.is_some()
            );
            match session.view_of_the_file(&json) {
                Some(view) => {
                    let body = view.root.descendants().find(|node| {
                        cpp_parser::CppSyntaxKind::from(node.kind())
                            == cpp_parser::CppSyntaxKind::ClassBody
                            && node.parent().is_some_and(|parent| {
                                parent.text().to_string().contains("class basic_json")
                            })
                    });
                    match body {
                        Some(body) => println!(
                            "[measure] view of json.hpp (its own text when no rendering is cached): {} child(ren), {} byte(s), {} parse error(s)",
                            body.children().count(),
                            view.source.len(),
                            view.tree.get_errors().len()
                        ),
                        None => println!("[measure] rendered basic_json ClassBody: not found"),
                    }
                }
                None => println!("[measure] no rendered view of json.hpp"),
            }
        }
        let cooked_reading = None::<()>;
        println!(
            "[cooked] cook returned: {}",
            if cooked_reading.is_some() {
                "a reading"
            } else {
                "none"
            }
        );
        match session.index().cooked_declarations(&json) {
            Some(cooked) => {
                let object = cooked.iter().find(|fact| {
                    fact.name.as_str() == "object"
                        && fact.scope.as_deref() == Some("nlohmann::basic_json")
                });
                println!(
                    "[cooked] json.hpp: {} declaration(s), object: {:?}",
                    cooked.len(),
                    object.map(|fact| fact.kind)
                );
            }
            None => println!("[cooked] json.hpp: no cooked reading"),
        }
    }
    // **What the index holds for the chain**, which is what a qualified lookup has to work with.
    for name in [
        "object",
        "basic_json::size",
        "basic_json::parse",
        "nlohmann::json",
        "nlohmann::basic_json",
        "nlohmann::basic_json::object",
        "nlohmann::json::object",
    ] {
        match session.index().definition(name, &path) {
            cpp_code_analysis::Known::Yes(found) => println!(
                "[index] {name}: kind {:?}, file {:?}, type {:?}, scope {:?}",
                found.fact.kind, found.file, found.fact.returns, found.fact.scope
            ),
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("[index] {name}: unknown {reason:?}")
            }
            cpp_code_analysis::Known::No => println!("[index] {name}: no"),
        }
    }

    // **The tokens, found in the rendering**: the last `object` of the `nlohmann::json::object()` call, and the
    // `items` of `SerializeArray(items)`.
    for (label, needle) in [
        ("json::object", "json::object"),
        ("SerializeArray(items)", "SerializeArray(items)"),
    ] {
        let Some(found) = rendering.rfind(needle) else {
            println!("{label}: not in the rendering");
            continue;
        };
        let at = if needle == "json::object" {
            found + "json::".len()
        } else {
            found + "SerializeArray(".len()
        };
        // **The plural answer too**, so a disagreement between the two queries is visible.
        match session.definitions(&view, ReadingOffset::new(at)) {
            cpp_code_analysis::Known::Yes(found) => println!(
                "[definitions] {:?}",
                found
                    .found
                    .iter()
                    .map(|definition| definition.fact.qualified_name())
                    .collect::<Vec<_>>()
            ),
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("[definitions] unknown: {reason:?}")
            }
            cpp_code_analysis::Known::No => println!("[definitions] no"),
        }
        // **What the walk sees now**, after the pump — the count is the first thing to read.
        {
            let in_scope = session
                .index()
                .declarations_in("nlohmann::basic_json", &path)
                .len();
            let by_name = session
                .index()
                .definition("nlohmann::basic_json::object", &path);
            println!(
                "[index now] declarations_in(basic_json) = {in_scope}, definition = {by_name:?}"
            );
        }
        // **What the session's own resolution says**, which is what the card is built from.
        match session.definition(&view, ReadingOffset::new(at)) {
            cpp_code_analysis::Known::Yes(found) => println!(
                "[definition] kind {:?}, scope {:?}, file {:?}",
                found.fact.kind, found.fact.scope, found.file
            ),
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("[definition] unknown: {reason:?}")
            }
            cpp_code_analysis::Known::No => println!("[definition] no"),
        }
        let around = rendering.get(found.saturating_sub(60)..(at + 40).min(rendering.len()));
        println!(
            "--- {label} at rendered {at} ---\n{}\n\n{}\n",
            around.unwrap_or(""),
            card(&session, &path, at)
        );
    }
}
