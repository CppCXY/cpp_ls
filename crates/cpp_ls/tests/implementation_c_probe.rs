//! **Does `textDocument/implementation` answer for a C project?** — a probe over Lua 5.5.0.
//!
//! ```text
//! $env:CPPLS_C_PROJECT = "C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0"
//! cargo test --release -p cpp_ls --test implementation_c_probe -- --ignored --nocapture
//! ```
//!
//! Reported from an editor: a function **declared in a C header** does not "go to implementation" — the cursor is on
//! `lua_State *lua_newstate (lua_Alloc f, void *ud);` in `lua.h`, the body is in `lstate.c`, and the feature answers
//! nothing. The handler's rule is a scan over the summaries for the same name whose text ends with `}` — that rule was
//! written for C++ out-of-line definitions, so the question is which step fails for C: the name lookup on the
//! declaration, the scope comparison, or the "is this a body" test.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::implementation::definitions_of;

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_C_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()),
    )
}

fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|it| it == "c" || it == "h") {
                files.push(path);
            }
        }
    }
    files
}

#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn a_c_header_declaration_answers_the_source_that_defines_it() {
    let project = project();
    let files = project_files(&project);
    eprintln!("[probe] {} project files", files.len());

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files(files.iter().cloned());
    session.index_everything();
    eprintln!(
        "[probe] {} summaries",
        session.index().all_summaries().len()
    );

    // The header, and the declarations in it that have a body elsewhere.
    for (header, names) in [
        (
            "src/lua.h",
            vec!["lua_newstate", "lua_gettop", "lua_settop", "lua_pcall"],
        ),
        ("src/ldebug.h", vec!["luaG_runerror", "luaG_typeerror"]),
        ("src/lstate.h", vec!["luaE_newthread", "luaE_freeCI"]),
    ] {
        let path = project.join(header);
        let Some(text) = std::fs::read_to_string(&path).ok() else {
            continue;
        };
        session.load(&path);
        let Some(held) = session.files().held(&path) else {
            continue;
        };
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        for name in names {
            let Some(at) = text.find(name).map(|at| at + 1) else {
                continue;
            };
            let found = session.definitions(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            );
            let asked = match &found {
                cpp_code_analysis::Known::Yes(known) => format!(
                    "{} fact(s): {:?}",
                    known.found.len(),
                    known
                        .found
                        .iter()
                        .map(|definition| (
                            definition.fact.name.to_string(),
                            definition.fact.kind,
                            definition.fact.scope.clone(),
                            definition.fact.definition
                        ))
                        .collect::<Vec<_>>()
                ),
                other => format!("{other:?}"),
            };
            let answered = definitions_of(&session, &view, at);
            eprintln!("[probe] {header}:{name} — declarations {asked}");
            eprintln!("[probe]   implementations answered {}:", answered.len());
            for location in &answered {
                eprintln!("[probe]     {location:?}");
            }
        }
    }
}

/// **What every summary says about one name** — the facts, and whether each one's text ends with a `}`.
#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn the_facts_of_one_c_function() {
    let project = project();
    let files = project_files(&project);
    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files(files.iter().cloned());
    session.index_everything();

    for name in ["lua_newstate", "lua_gettop", "luaG_runerror"] {
        eprintln!("[probe] ===== {name}");
        let summaries: Vec<(std::path::PathBuf, cpp_code_analysis::FileSummary)> = session
            .index()
            .all_summaries()
            .iter()
            .map(|(path, summary)| (path.clone(), (*summary).clone()))
            .collect();
        for (path, summary) in &summaries {
            for fact in &summary.declarations {
                if fact.name != name {
                    continue;
                }
                let text = std::fs::read_to_string(path).ok();
                let body = text
                    .as_deref()
                    .and_then(|text| text.get(fact.range.start_offset..fact.range.end_offset()));
                eprintln!(
                    "[probe]   {}: kind {:?} scope {:?} definition {} range {}..{} ends_with }} {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    fact.kind,
                    fact.scope,
                    fact.definition,
                    fact.range.start_offset,
                    fact.range.end_offset(),
                    body.map(|text| text.trim_end().ends_with('}'))
                        .unwrap_or(false)
                );
                if let Some(body) = body.filter(|body| body.len() < 200) {
                    eprintln!("[probe]     text {:?}", body);
                }
            }
        }
    }
}
