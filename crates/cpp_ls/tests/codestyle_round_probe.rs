//! **A probe for the four `EmmyLuaCodeStyle` reports of this round.**
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test codestyle_round_probe -- --ignored --nocapture
//! ```
//!
//! It answers, in order:
//!
//! 1. **The cooked text of `json.hpp`**, written to `target/rendering-json.hpp.txt`, so the parse that loses the
//!    class body's tail can be read as the compiler's view rather than as the file's own text.
//! 2. **Which members the cooked reading files** for `nlohmann::basic_json`, by the line they are written on — the
//!    report is that `operator[]` is not among them, and the line is where the body stops being read.
//! 3. **The type of `it`** in `LuaStyle.cpp` (`auto it = configMap.find(key)`) and the card for `it->second`, which is
//!    the second report: the iterator type is `_Tree_const_iterator<_Scary_val>`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle".to_string()),
    )
}

fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![
        root.join("CodeFormatServer/src"),
        root.join("CodeFormatCore/src"),
        root.join("LuaParser/src"),
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

#[test]
#[ignore = "a probe: opens a real project and prints instead of asserting"]
fn the_round_three_reports() {
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
    // **The pump is bounded by an environment variable, and it reports what it holds.** This probe indexes the
    // project's whole include closure — the standard library and the Windows SDK — and the session keeps the
    // summaries, the read texts and every rendering it makes; run unbounded on a cold cache it was measured at
    // **5.1 GB** of working set in one process, which is a fact about the pump rather than about this report (see
    // `docs/memory-status.md`). `CPPLS_PROBE_SLICES` is the bound, and the report after each tenth slice says
    // which bucket is growing.
    let limit = std::env::var("CPPLS_PROBE_SLICES")
        .ok()
        .and_then(|written| written.parse::<usize>().ok())
        .unwrap_or(400);
    let started = Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < limit {
        session.advance(64);
        slices += 1;
        if slices.is_multiple_of(10) {
            let report = session.memory_report();
            println!(
                "[memory] {slices}: {:.0} MB held (summaries {:.0}, vfs {:.0}, cooked {:.0}, units {:.0})",
                report.total() as f64 / 1e6,
                report.summary_bytes as f64 / 1e6,
                report.vfs_text_bytes as f64 / 1e6,
                report.cooked_bytes as f64 / 1e6,
                report.unit_bytes as f64 / 1e6
            );
        }
    }
    println!(
        "pumped {slices} slice(s) in {:?}, {} work item(s) left",
        started.elapsed(),
        session.pending_work()
    );
    {
        let report = session.memory_report();
        println!(
            "[memory] after the pump: {:.0} MB held (summaries {:.0}, vfs {:.0}, cooked {:.0}, units {:.0}, \
             renderings {:.0}, views {:.0}, index tables {:.0})",
            report.total() as f64 / 1e6,
            report.summary_bytes as f64 / 1e6,
            report.vfs_text_bytes as f64 / 1e6,
            report.cooked_bytes as f64 / 1e6,
            report.unit_bytes as f64 / 1e6,
            report.rendering_bytes as f64 / 1e6,
            report.view_bytes as f64 / 1e6,
            report.index_table_bytes() as f64 / 1e6
        );
    }

    // **1. The cooked text of json.hpp**, so the parse of *it* can be inspected with `inspect`.
    let json = root.join("3rd/nlohmann_json/include/nlohmann/json.hpp");
    let cooked = session.cook(&json);
    println!("[cook] json.hpp -> {cooked:?}");
    if let Some(rendered) = session.rendered_text_of(&json) {
        let out = PathBuf::from(r"C:\Users\zc\Desktop\github\cpp_ls\target\rendering-json.hpp.txt");
        let _ = std::fs::write(&out, &rendered);
        println!(
            "[cook] rendered text: {} byte(s) -> {}",
            rendered.len(),
            out.display()
        );
        // **The parse of that text, as `index_rendering` parses it** — the errors are placed in the rendering, so
        // the text around each one is what a reader needs to see the shape.
        let tree = cpp_parser::CppParser::parse(&rendered, cpp_parser::ParserConfig::default());
        let errors = tree.get_errors();
        println!("[parse] {} error(s):", errors.len());
        for error in errors.iter().take(12) {
            let range = cpp_parser::source_range(error.range);
            let at = range.start_offset;
            let around = rendered.get(at.saturating_sub(90)..(at + 90).min(rendered.len()));
            println!(
                "  [{}..{}] {}  {:?}",
                at,
                range.end_offset(),
                error.message,
                around.unwrap_or("")
            );
        }
        // **Where the class body ends**, and how many children it has.
        let root_node = tree.get_red_root();
        let body = root_node.descendants().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ClassBody
                && node
                    .parent()
                    .is_some_and(|parent| parent.text().to_string().contains("basic_json"))
        });
        match body {
            Some(body) => {
                println!(
                    "[parse] basic_json ClassBody: {} child(ren), {}..{}",
                    body.children().count(),
                    u32::from(body.text_range().start()),
                    u32::from(body.text_range().end())
                );
                let tail = body.text().to_string();
                println!(
                    "[parse] ClassBody tail: {:?}",
                    tail.get(tail.len().saturating_sub(200)..).unwrap_or("")
                );
            }
            None => println!("[parse] basic_json ClassBody: not found"),
        }
        // **Which scope the walk gives the `operator[]` member** — the fact is unscoped, so the question is whether
        // the scope walk or the fact builder loses it. Printed as the chain from the file scope inward.
        let scopes = cpp_code_analysis::build_scopes(&root_node, &cpp_code_analysis::NoMacroBodies);
        let ops_at = rendered
            .find("reference operator[](size_type idx)")
            .unwrap_or_default();
        let mut containing: Vec<(usize, String)> = scopes
            .scopes()
            .iter()
            .filter_map(|scope| {
                let range = scope.range?;
                (range.start_offset <= ops_at && ops_at < range.end_offset()).then_some((
                    range.end_offset() - range.start_offset,
                    format!(
                        "{:?}{}",
                        scope.kind,
                        scope
                            .name
                            .as_ref()
                            .map(|name| format!(" `{name}`"))
                            .unwrap_or_default()
                    ),
                ))
            })
            .collect();
        containing.sort();
        println!(
            "[scopes] at `operator[]` ({ops_at}): {} scope(s), innermost {}",
            containing.len(),
            containing.first().map_or("none", |(_, name)| name.as_str())
        );
        for (span, name) in containing.iter().take(4) {
            println!("  {name} ({span} byte(s))");
        }
        // **Where the binding lives**, which is what `build_facts` turns into a scope: the range containment above
        // is a proxy, and a scope whose *parent* is the file scope is a different answer from one under the class.
        for (index, scope) in scopes.scopes().iter().enumerate() {
            for binding in &scope.bindings {
                let name = binding.name.text();
                if !name.contains("operator[]") {
                    continue;
                }
                let mut chain = Vec::new();
                let mut current = Some(cpp_code_analysis::sema::symbol::ScopeId(index));
                while let Some(id) = current {
                    let Some(scope) = scopes.scope(id) else { break };
                    chain.push(format!(
                        "{:?}{}",
                        scope.kind,
                        scope
                            .name
                            .as_ref()
                            .map(|name| format!(" `{name}`"))
                            .unwrap_or_default()
                    ));
                    current = scope.parent;
                }
                println!(
                    "[bindings] `{name}` in scope {index}, chain: {}",
                    chain.join(" < ")
                );
            }
        }
        // **The facts built from this very text**, so "the cache holds an older reading" and "the fact builder
        // loses the scope" are told apart: the same two questions are asked of a reading built here.
        let preprocessing = cpp_code_analysis::preprocess(&rendered, tree.get_tokens());
        let (fresh_facts, _guards) =
            cpp_code_analysis::build_facts(&scopes, &preprocessing, &root_node, &[], true);
        for wanted in ["operator[]", "at", "is_string"] {
            let found: Vec<&cpp_code_analysis::DeclFact> = fresh_facts
                .iter()
                .filter(|fact| fact.name.as_str() == wanted)
                .collect();
            println!(
                "[fresh] `{wanted}`: {} fact(s), scopes {:?}",
                found.len(),
                found
                    .iter()
                    .map(|fact| fact
                        .scope
                        .as_ref()
                        .map_or("(none)".to_string(), |scope| scope.to_string()))
                    .collect::<Vec<_>>()
            );
        }
    } else {
        println!("[cook] no rendered text for json.hpp");
    }

    // **The text the index's facts were actually built from.** `rendered_text_of` recomputes a rendering from the
    // session's current state; the facts came from the cook the disk stored, and that record carries its own
    // rendering. Parsing *this* text is what can explain a fact whose scope was lost.
    if let Some((_stored_path, stored_file, stored)) = session.a_stored_cook(&json) {
        let text = &stored.text;
        let out = PathBuf::from(r"C:\Users\zc\Desktop\github\cpp_ls\target\stored-json.hpp.txt");
        let _ = std::fs::write(&out, text);
        println!(
            "[stored] {} declaration(s), rendering {} byte(s) -> {}",
            stored_file.declarations.len(),
            text.len(),
            out.display()
        );
        let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
        println!("[stored] {} error(s)", tree.get_errors().len());
        for error in tree.get_errors().iter().take(8) {
            let range = cpp_parser::source_range(error.range);
            let at = range.start_offset;
            let around = text.get(at.saturating_sub(80)..(at + 80).min(text.len()));
            println!("  [{}] {}", at, error.message);
            println!("    {:?}", around.unwrap_or(""));
        }
        let root_node = tree.get_red_root();
        let body = root_node.descendants().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ClassBody
                && node
                    .parent()
                    .is_some_and(|parent| parent.text().to_string().contains("basic_json"))
        });
        if let Some(body) = body {
            println!(
                "[stored] basic_json ClassBody: {} child(ren), {}..{}",
                body.children().count(),
                u32::from(body.text_range().start()),
                u32::from(body.text_range().end())
            );
        }
        let scopes = cpp_code_analysis::build_scopes(&root_node, &cpp_code_analysis::NoMacroBodies);
        let preprocessing = cpp_code_analysis::preprocess(text, tree.get_tokens());
        let (stored_facts, _) =
            cpp_code_analysis::build_facts(&scopes, &preprocessing, &root_node, &[], true);
        for wanted in ["operator[]", "at", "is_string"] {
            let found: Vec<&cpp_code_analysis::DeclFact> = stored_facts
                .iter()
                .filter(|fact| fact.name.as_str() == wanted)
                .collect();
            println!(
                "[stored] `{wanted}`: {} fact(s), scopes {:?}",
                found.len(),
                found
                    .iter()
                    .map(|fact| fact
                        .scope
                        .as_ref()
                        .map_or("(none)".to_string(), |scope| scope.to_string()))
                    .collect::<Vec<_>>()
            );
        }
        println!(
            "[stored] {} fact(s) built from that text",
            stored_facts.len()
        );
    } else {
        println!("[stored] the disk holds no cook for json.hpp");
    }

    // **Which environment a cook of json.hpp would use now**: a held translation unit (the accurate one) or the
    // environment composed from the index (the fallback, which is what left `JSON_TRY` unexpanded in the stored
    // rendering).
    match session.cooking_materials(&json) {
        Some(materials) => {
            println!(
                "[materials] json.hpp: unit {}, composed configuration {}",
                materials.unit.is_some(),
                materials.configuration.is_some()
            );
            // **What the composed table holds**, and what a fresh render of it makes of the two macros the class
            // body's tail depends on.
            if let Some(configuration) = &materials.configuration {
                println!(
                    "[materials] composed table: {} name(s); JSON_TRY {}, JSON_CATCH {}; seed {} name(s)",
                    configuration.table.len(),
                    configuration.table.get("JSON_TRY").is_some(),
                    configuration.table.get("JSON_CATCH").is_some(),
                    materials.seed.len()
                );
            }
            if let Some(rendered) = session.render_a_cooked(&materials) {
                println!(
                    "[render] composed now: {} declaration(s), {} diagnostic(s), {} unplaced",
                    rendered.reading.declarations,
                    rendered.reading.diagnostics,
                    rendered.reading.unplaced
                );
                // **What the class's members look like in that reading** — the report is that `operator[]` is not
                // among them, and that the members after `at` have no scope.
                let facts = &rendered.indexed.summary.declarations;
                let members: Vec<&str> = facts
                    .iter()
                    .filter(|fact| fact.scope.as_deref() == Some("nlohmann::basic_json"))
                    .map(|fact| fact.name.as_str())
                    .collect();
                println!(
                    "[render] basic_json: {} member(s); operator[] {}, is_string {}, unscoped {}",
                    members.len(),
                    members.iter().filter(|name| **name == "operator[]").count(),
                    members.iter().filter(|name| **name == "is_string").count(),
                    facts.iter().filter(|fact| fact.scope.is_none()).count()
                );
                // **File that reading and ask the reported question** — the point of the whole measurement: does
                // `json["data"].is_string()` answer once the class body is read whole? `commit_a_cooked` is the same
                // insert the pump makes; the hover is the handler's own.
                if std::env::var_os("CPPLS_PROBE_ASK").is_some() {
                    let committed = session.commit_a_cooked(rendered);
                    println!("[commit] {committed:?}");
                    for (label, cook_first) in [("after the commit", false), ("after a cook", true)]
                    {
                        if cook_first {
                            let _ = session.cook(&json);
                        }
                        let view = session.view_of_the_file(&path).expect("LSP.cpp has a view");
                        if cook_first {
                            // **What the index answers after the reading is filed**: the member count, and whether
                            // the subscript is among the members — the two things the hover's chain needs.
                            let listed = session.members_of(&view, "nlohmann::basic_json");
                            let listed: Vec<String> = match &listed {
                                cpp_code_analysis::Known::Yes(list) => list
                                    .members
                                    .iter()
                                    .map(|member| member.fact.name.to_string())
                                    .collect(),
                                other => vec![format!("{other:?}")],
                            };
                            println!(
                                "[ask] members_of(basic_json) = {} member(s); operator[] {}",
                                listed.len(),
                                listed.iter().filter(|name| *name == "operator[]").count()
                            );
                            println!(
                                "[ask] definition(basic_json::operator[]) = {:?}",
                                session
                                    .index()
                                    .definition("nlohmann::basic_json::operator[]", &path)
                            );
                            // **The fact the *card* would use** when it comes from the member list, and the two
                            // spellings a function's return type has.
                            if let cpp_code_analysis::Known::Yes(listed) =
                                session.members_of(&view, "nlohmann::basic_json")
                                && let Some(member) = listed
                                    .members
                                    .iter()
                                    .find(|member| member.fact.name == "is_string")
                            {
                                println!(
                                    "[ask] member is_string: returns {:?}, returns_in_the_class {:?}, range {:?}",
                                    member.fact.returns,
                                    member.fact.returns_in_the_class(),
                                    member.fact.range
                                );
                            }
                        }
                        let haystack = view.source.to_string();
                        let Some(at) = haystack.find("is_string") else {
                            continue;
                        };
                        let reading = cpp_code_analysis::file::coordinates::ReadingOffset::new(at);
                        let file = view.file_offset_of(reading).unwrap_or_else(|| {
                            cpp_code_analysis::file::coordinates::FileOffset::new(at)
                        });
                        // **The fact the card is built from**, so a card whose type is not the fact's can be told
                        // from a fact that is wrong.
                        if cook_first {
                            match session.definition(&view, reading) {
                                cpp_code_analysis::Known::Yes(found) => println!(
                                    "[ask] definition: returns {:?}, range {:?}, file {}",
                                    found.fact.returns,
                                    found.fact.range,
                                    found.file.display()
                                ),
                                other => println!("[ask] definition: {other:?}"),
                            }
                        }
                        println!(
                            "--- hover on `is_string` {label} ---\n{}",
                            match cpp_ls::handlers::hover::hover(&session, &view, reading, file) {
                                None => "NO CARD".to_string(),
                                Some(hover) => match hover.contents {
                                    lsp_types::HoverContents::Markup(markup) => markup.value,
                                    other => format!("{other:?}"),
                                },
                            }
                        );
                    }
                }
            }
            // **The memory question this probe had to answer for itself**: an unbounded pump on this project's
            // closure was measured at 5.1 GB, and it is the pump, not this composition.
            let report = session.memory_report();
            println!(
                "[memory] after the composition: {:.0} MB held (summaries {:.0}, vfs {:.0})",
                report.total() as f64 / 1e6,
                report.summary_bytes as f64 / 1e6,
                report.vfs_text_bytes as f64 / 1e6
            );
            if std::env::var_os("CPPLS_PROBE_ONLY_MATERIALS").is_some() {
                println!("[probe] stopping after the materials section, as asked");
                return;
            }
        }
        None => println!("[materials] json.hpp: no materials"),
    }
    // **And the state an editor is in**: the open file's own unit is what a header included by it is cooked
    // through. Cooking `LSP.cpp` builds and holds that unit; the question is whether the header then has one.
    let _ = session.cook(&path);
    match session.cooking_materials(&json) {
        Some(materials) => println!(
            "[materials] after cooking the open file: json.hpp unit {}, composed configuration {}",
            materials.unit.is_some(),
            materials.configuration.is_some()
        ),
        None => println!("[materials] after cooking the open file: no materials"),
    }

    // **The file the two macros come from**: does its summary hold them, and behind which guard?
    {
        let scope = root.join("3rd/nlohmann_json/include/nlohmann/detail/macro_scope.hpp");
        match session.index().summary(&scope) {
            Some(summary) => {
                println!(
                    "[scope] {} macro fact(s), {} region(s), own guard {:?}",
                    summary.macros.len(),
                    summary.guards.regions.len(),
                    summary.guards.own_guard
                );
                for fact in summary.macros.iter().filter(|fact| {
                    fact.name.starts_with("JSON_TRY") || fact.name.starts_with("JSON_CATCH")
                }) {
                    println!(
                        "[scope] `{}` guard {:?}, body {:?}, at {}",
                        fact.name,
                        fact.guard,
                        fact.body_range.is_some(),
                        fact.range.start_offset
                    );
                }
                // Is the file in the closure the walk would take, at all?
                let mut found = false;
                let mut pending = vec![json.clone()];
                while let Some(next) = pending.pop() {
                    if next == scope {
                        found = true;
                        break;
                    }
                    if let Some(summary) = session.index().summary(&next) {
                        pending.extend(
                            summary
                                .includes
                                .iter()
                                .filter_map(|include| include.resolved.clone()),
                        );
                    }
                }
                println!("[scope] reachable from json.hpp by includes: {found}");
            }
            None => println!("[scope] no summary for macro_scope.hpp"),
        }
    }

    // **2. What the cooked reading filed for the class**, by the line it is written on.
    if let Some(facts) = session.index().cooked_declarations(&json) {
        let mut members: Vec<(String, usize)> = facts
            .iter()
            .filter(|fact| fact.scope.as_deref() == Some("nlohmann::basic_json"))
            .map(|fact| (fact.name.to_string(), fact.name_range.start_offset))
            .collect();
        members.sort_by_key(|(_, offset)| *offset);
        let written = std::fs::read_to_string(&json).unwrap_or_default();
        let line_of = |offset: usize| written[..offset.min(written.len())].lines().count();
        println!("[cooked] {} declaration(s) in all", facts.len());
        println!("[cooked] {} member(s) of basic_json, tail:", members.len());
        for (name, offset) in members.iter().rev().take(8).rev() {
            println!("  {} line {}", name, line_of(*offset));
        }
        for wanted in ["operator[]", "operator ValueType", "is_string", "at"] {
            let found: Vec<String> = members
                .iter()
                .filter(|(name, _)| name == wanted)
                .map(|(_, offset)| format!("line {}", line_of(*offset)))
                .collect();
            println!("[cooked] `{wanted}`: {found:?}");
        }
        // **Where the class's members stop and what happens to the ones after** — a member whose scope is `None` is
        // read by the parse and not attributed to the class, which is a different defect from a lost parse.
        println!("[cooked] declarations by line, 3470..3760:");
        let mut between: Vec<(usize, String, String)> = facts
            .iter()
            .map(|fact| {
                (
                    line_of(fact.name_range.start_offset),
                    fact.name.to_string(),
                    fact.scope
                        .as_ref()
                        .map_or("(none)".to_string(), |scope| scope.to_string()),
                )
            })
            .filter(|(line, _, _)| (3470..3760).contains(line))
            .collect();
        between.sort();
        for (line, name, scope) in &between {
            println!("  line {line}: {scope}::{name}");
        }
        let unscoped = facts.iter().filter(|fact| fact.scope.is_none()).count();
        println!("[cooked] {unscoped} declaration(s) with no scope");
    } else {
        println!("[cooked] no cooked declarations for json.hpp");
    }

    // **3. The `LuaStyle.cpp` questions**: `it`, and the member `second` on it.
    let style = root.join("CodeFormatCore/src/Config/LuaStyle.cpp");
    let style_text = std::fs::read_to_string(&style).expect("LuaStyle.cpp is here");
    session.did_open(&style, &style_text);
    let mut style_slices = 0usize;
    while session.pending_work() > 0 && style_slices < 2000 {
        session.advance(64);
        style_slices += 1;
    }
    let view = session.view_of_the_file(&style).expect("a view");
    let at = view.source.to_string();
    let it_at = at.find("auto it = configMap.find(key);").expect("the line") + 5;
    let second_at = at.find("return it->second;").expect("the line") + "return it->".len();
    for (label, offset) in [("it (the declarator)", it_at), ("second", second_at)] {
        let reading = ReadingOffset::new(offset);
        let file = view
            .file_offset_of(reading)
            .unwrap_or_else(|| FileOffset::new(offset));
        println!("--- {label} at {offset} ---");
        println!(
            "{}",
            match cpp_ls::handlers::hover::hover(&session, &view, reading, file) {
                None => "NO CARD".to_string(),
                Some(hover) => match hover.contents {
                    lsp_types::HoverContents::Markup(markup) => markup.value,
                    other => format!("{other:?}"),
                },
            }
        );
        println!("type: {:?}", session.type_at(&view, reading));
    }
}
