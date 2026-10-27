//! **What the analysis answers at a place in a real file** — the probe behind the editor reports, driven entirely by
//! the environment so that no project path is baked into the code.
//!
//! ```text
//! CPPLS_PROJECT_ROOT=<a project> CPPLS_FILE=<a file in it> CPPLS_NEEDLE=<text to look for> \
//!   cargo test --release -p cpp_code_analysis --test inference_probe -- --ignored --nocapture
//! ```
//!
//! For every `needle` found in the file it prints: the member list after the `.`/`->` that follows it, the member
//! answer, and the definitions a cursor on it gets. That is the shape of both reports this exists for: a subscript
//! whose element type is lost (`events[Pos].U`) and a smart pointer whose members are missing (`file->setUri`).

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::{Path, PathBuf};

#[test]
#[ignore = "opens a real project; run with --ignored and CPPLS_PROJECT_ROOT"]
fn what_is_answered_at_a_place() {
    let root = PathBuf::from(
        std::env::var("CPPLS_PROJECT_ROOT").expect("CPPLS_PROJECT_ROOT names the project"),
    );
    let file = PathBuf::from(std::env::var("CPPLS_FILE").expect("CPPLS_FILE names a file in it"));
    let needle = std::env::var("CPPLS_NEEDLE").unwrap_or_else(|_| ".".to_string());
    let text = std::fs::read_to_string(&file).expect("the file is readable");

    // **Opened the way the server opens it**, not with a caller's configuration: the project's own file
    // (`compile_commands.json`, `.cppls.toml`, the compiler's predefined macros) is what decides whether an include
    // resolves — and a probe that skips it answers about a project nobody analyses. Measured on this one: with
    // `with_config` and no database, `p.GetEvents()` typed as `Unknown(NotDeclaredHere("LuaParser::GetEvents"))`;
    // the header is indexed, it is simply not *visible* from a file whose include does not resolve.
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let started = std::time::Instant::now();
    session.did_open(&file, &text);

    // **A cooked reading on purpose**: the checks are run over the compiler's view, and a probe that asks for
    // diagnostics without one is answered "nothing to say" rather than "nothing wrong".
    session.want_cooked_reading(&file);
    session.add_project_files([file.clone()]);
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    println!(
        "indexed {} file(s) in {:?}",
        session.cache_sizes().summaries,
        started.elapsed()
    );

    // **What the check layer says about the whole file**, when asked: a false positive is a report about
    // diagnostics rather than about one place, and the code carries the rule's own name.
    if std::env::var_os("CPPLS_DIAGNOSTICS").is_some() {
        match session.diagnostics(&file) {
            Some(found) => {
                println!("{} finding(s):", found.checks.len() + found.errors.len());
                for finding in &found.checks {
                    println!(
                        "  {:?} at {}: {}",
                        finding.code, finding.range.start_offset, finding.message
                    );
                }
                for error in &found.errors {
                    println!(
                        "  error {:?} at {} (length {}): {}",
                        error.code, error.start, error.end, error.message
                    );
                    // **The text the error is about.** A rendering is one long line, so the offset alone says nothing
                    // a reader can act on; ±60 bytes of the rendering is what turns "expected primary expression at
                    // 19653" into a name.
                    if let Some(rendered) = session.rendering_of(&file) {
                        let start = error.start.min(rendered.text.len());
                        let end = error.end.min(rendered.text.len()).max(start);
                        let from = start.saturating_sub(50);
                        let to = (end + 50).min(rendered.text.len());
                        if rendered.text.is_char_boundary(from)
                            && rendered.text.is_char_boundary(to)
                            && rendered.text.is_char_boundary(start)
                            && rendered.text.is_char_boundary(end)
                        {
                            println!(
                                "        …{}⟦{}⟧{}…",
                                &rendered.text[from..start].replace('\n', " "),
                                &rendered.text[start..end].replace('\n', " "),
                                &rendered.text[end..to].replace('\n', " "),
                            );
                        }
                    }
                }
            }
            None => println!("the check layer had nothing to say about the file"),
        }
    }

    // **The text the parser was handed**, when asked — the one artifact that separates "the checker is wrong" from
    // "the preprocessor handed the parser something that is not a program". A file whose macros do not expand reads
    // as a pile of syntax errors, and every one of them is an accurate report about the text it was shown.
    if std::env::var_os("CPPLS_RENDERING").is_some() {
        match session.rendering_of(&file) {
            Some(rendered) => {
                let lines = std::env::var("CPPLS_RENDERING_LINES")
                    .ok()
                    .and_then(|count| count.parse::<usize>().ok())
                    .unwrap_or(40);
                println!("--- the rendering, first {lines} line(s) ---");
                for line in rendered.text.lines().take(lines) {
                    println!("{line}");
                }
                // **The text around one offset**, for a diagnostic that names a *rendering* offset: a syntax error's
                // range is in the rendering's coordinates, and a rendering is one long line, so "the first N lines"
                // cannot show it. `CPPLS_RENDERING_AT=<offset>` prints ±200 bytes instead.
                if let Ok(at) = std::env::var("CPPLS_RENDERING_AT") {
                    if let Ok(at) = at.parse::<usize>() {
                        let from = at.saturating_sub(200);
                        let to = (at + 200).min(rendered.text.len());
                        println!(
                            "--- the rendering at {at} (bytes {from}..{to}) ---\n{}",
                            &rendered.text[from..to]
                        );
                    }
                }
            }
            None => println!("the file has no rendering yet"),
        }
    }

    // **Is the rendering itself a program?** The check layer's syntax errors are ranges in the rendering, and the
    // question "is the parser wrong or is the text it was handed wrong" cannot be answered from the ranges alone.
    // This parses the very text `rendering_of` hands out and prints its own errors: a text that reads clean here was
    // *not* the text the checks saw, and the two renderings then differ.
    if std::env::var_os("CPPLS_REPARSE").is_some() {
        match session.rendering_of(&file) {
            Some(rendered) => {
                let tree = cpp_parser::CppParser::parse(
                    &rendered.text,
                    cpp_parser::ParserConfig::default(),
                );
                let errors = tree
                    .get_red_root()
                    .descendants()
                    .filter(|node| {
                        cpp_parser::CppSyntaxKind::from(node.kind())
                            == cpp_parser::CppSyntaxKind::ErrorNode
                    })
                    .count();
                println!(
                    "--- re-parsing the rendering ({} byte(s)): {errors} error node(s)",
                    rendered.text.len()
                );
                for node in tree.get_red_root().descendants().filter(|node| {
                    cpp_parser::CppSyntaxKind::from(node.kind())
                        == cpp_parser::CppSyntaxKind::ErrorNode
                }) {
                    let start = usize::from(node.text_range().start());
                    let end = usize::from(node.text_range().end());
                    let from = start.saturating_sub(50);
                    let to = (end + 50).min(rendered.text.len());
                    println!(
                        "  …{}⟦{}⟧{}…",
                        &rendered.text[from..start].replace('\n', " "),
                        &rendered.text[start..end].replace('\n', " "),
                        &rendered.text[end..to].replace('\n', " "),
                    );
                }
            }
            None => println!("the file has no rendering yet"),
        }
    }

    // **And every fact the index holds for a name**, when one is named: the `returns` a call has to answer with.
    if let Ok(name) = std::env::var("CPPLS_NAME") {
        for summary in session.index().summaries() {
            for fact in &summary.declarations {
                if fact.name == name {
                    println!(
                        "{name} in {}: kind {:?} scope {:?} returns {:?} type_of {:?} parameters {:?}",
                        summary.path.display(),
                        fact.kind,
                        fact.scope,
                        fact.returns,
                        fact.type_of,
                        fact.parameters
                    );
                }
            }
        }
    }

    let view = session.view_of_the_file(&file).expect("a view of the file");
    let mut at = 0;
    while let Some(found) = text[at..].find(&needle) {
        // **Where the cursor is inside the needle**: at its start by default, at its last character with
        // `CPPLS_AT=last` — the difference matters for a question about an *expression* (`_lineOffsetVec[line]`)
        // rather than about the name it begins with.
        let start = at + found;
        let offset = match std::env::var("CPPLS_AT").as_deref() {
            Ok("last") => start + needle.trim_end().len().saturating_sub(1),
            _ => start,
        };
        at = start + needle.len();
        let line = text[..offset].lines().count();
        let snippet: String = text[offset..]
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(70)
            .collect();

        // The member list at the end of the needle, when the needle ends with `.` or `->`.
        let cursor = offset + needle.len();
        let completions = session.member_completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(cursor),
        );
        match completions {
            Known::Yes(list) => {
                let names: Vec<&str> = list
                    .members
                    .members
                    .iter()
                    .map(|member| member.fact.name.as_str())
                    .take(8)
                    .collect();
                println!(
                    "{line}: …{snippet}\n     class {:?}: {} member(s) {names:?}",
                    list.class,
                    list.members.members.len()
                );
            }
            other => println!("{line}: …{snippet}\n     completions: {other:?}"),
        }

        // **What macro the name at the cursor is**, when `CPPLS_MACRO=1`: the cross-file answer a hover on a macro
        // shows. A macro is not a declaration, so `definitions` says nothing about it.
        if std::env::var_os("CPPLS_MACRO").is_some() {
            let found = session.macro_definition(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
            );
            println!("     macro: {found:?}");
        }

        // And what the cursor on the needle's last character resolves to.
        let definitions = session.definitions(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
        );
        if let Known::Yes(found) = definitions {
            let names: Vec<String> = found
                .found
                .iter()
                .map(|one| {
                    format!(
                        "{}::{}",
                        one.fact.scope.as_deref().unwrap_or(""),
                        one.fact.name
                    )
                })
                .collect();
            println!("     definitions: {names:?}");
        }

        // **What the expression at the cursor is**, which is the question a hover on an `auto` variable ends in:
        // `type_at` reads the initializer when the declaration has no type of its own. `CPPLS_TYPE=1`.
        if std::env::var_os("CPPLS_TYPE").is_some() {
            let typed = session.type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
            );
            println!("     type_at: {typed:?}");
        }
    }

    let _ = Path::new("");
}
