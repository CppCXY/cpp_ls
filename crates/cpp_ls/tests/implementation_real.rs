//! **Does `textDocument/implementation` work on the real project?** — the session-level half of the question.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_ls --test implementation_real -- --ignored --nocapture
//! ```
//!
//! The end-to-end probe (`lsp_status.rs`) answered `null` for this feature on the real project while the fixture test
//! answered correctly, so the question is which half is wrong: the handler's logic over this project's facts, or the
//! server path around it (the URI, the position, the cooked reading). This asks the logic directly — the same
//! `definitions_of` the handler calls, with a `Session` over the disk and the file's own text — so a failure here is
//! the index or the query, and a pass here is the handler's surroundings.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::implementation::definitions_of;

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn a_header_declaration_answers_the_source_that_defines_it() {
    let project = project();
    // The header and the source of one class, and a function the project defines out of line.
    let header = project.join("CodeFormatCLib/src/codeformat.h");
    let source = project.join("CodeFormatCLib/src/CodeFormat.cpp");
    let header_text = std::fs::read_to_string(&header).expect("the header reads");

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([source.clone()]);
    session.index_everything();

    // **Read it before borrowing it**: `load` takes `&mut Session`, `held` a shared borrow.
    session.load(&header);
    let Some(held) = session.files().held(&header) else {
        panic!("the header is readable");
    };
    let view =
        cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());

    // **Every declaration of `UpdateCodeStyle` in the header**, asked one at a time: the first occurrence is the one a
    // reader's cursor would be on, and a comment before it is why the probe asks the *declaration* explicitly.
    let mut asked = 0;
    // **A byte offset, counted from the text itself.** The first version summed line lengths with `+ 1` per line — and
    // this project's files use CRLF, so every offset was one byte per preceding line too early: it read a *different*
    // declaration (`CodeFormat`, the class) and answered 0 locations, which looked exactly like a broken rule.
    for (at, _) in header_text.match_indices("UpdateCodeStyle") {
        let line = header_text[..at].matches('\n').count();
        let column = at
            - header_text[..at]
                .rfind('\n')
                .map_or(0, |newline| newline + 1);
        let offset = at + 1;
        asked += 1;
        // **A second shape**: the definition put the returned type in front of a qualified name in the *declarator*
        // (`std::vector<LuaTokenError> &LuaLexer::GetErrors()`), and the scope walker had filed it as a plain function of
        // the file — `scope: None` — so the header's declaration found nothing. It is the fact the walker made, adopted
        // and given its scope (see `qualified_declarations`), rather than a second fact.
        let lexer_header = project.join("LuaParser/include/LuaParser/Lexer/LuaLexer.h");
        let lexer_source = project.join("LuaParser/src/Lexer/LuaLexer.cpp");
        if lexer_header.exists() && lexer_source.exists() {
            let header = std::fs::read_to_string(&lexer_header).expect("the header reads");
            let Some(raw) = header.find("GetErrors").map(|at| at + 1) else {
                panic!("the header declares GetErrors");
            };
            let mut session = Session::open(
                project.clone(),
                SessionFiles::new(OpenDocuments::new(), DiskFiles),
                WatchFilter::new(&project),
            );
            session.add_project_files([lexer_header.clone(), lexer_source.clone()]);
            session.index_everything();
            let view = session
                .view_of_the_file(&lexer_header)
                .expect("the header parses");
            let found = definitions_of(
                &session,
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(raw).get(),
            );
            println!(
                "GetErrors at header offset {raw} → {:?}",
                found
                    .iter()
                    .map(|location| format!(
                        "{}:{}",
                        location.uri.path(),
                        location.range.start.line + 1
                    ))
                    .collect::<Vec<_>>()
            );
            assert!(
                found
                    .iter()
                    .any(|location| location.uri.path().ends_with("LuaLexer.cpp")),
                "the definition in the .cpp: {found:?}"
            );
        }
        let found = definitions_of(&session, &view, offset);
        println!(
            "line {} col {} → {} location(s): {:?}",
            line + 1,
            column,
            found.len(),
            found
                .iter()
                .map(|location| format!(
                    "{}:{}",
                    location.uri.path(),
                    location.range.start.line + 1
                ))
                .collect::<Vec<_>>()
        );
        assert!(
            !found.is_empty(),
            "`{}` declares `UpdateCodeStyle` and `{}` defines it out of line",
            header.display(),
            source.display()
        );
        assert!(
            found.iter().any(|location| location
                .uri
                .path()
                .to_ascii_lowercase()
                .ends_with("codeformat.cpp")),
            "the implementation is in the source file: {found:?}"
        );
    }
    assert!(asked > 0, "the fixture names the function");
}
