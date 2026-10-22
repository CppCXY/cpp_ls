//! TEMPORARY: what a "browse parser" (token scan, no tree, no preprocessing) costs on this project.
use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_parser::{CppTokenKind, LexerConfig};

#[test]
fn temp_fuzzy_index() {
    let root = PathBuf::from("C:/Users/zc/Desktop/github/EmmyLuaCodeStyle");
    let files = SessionFiles::new(OpenDocuments::new(), DiskFiles);
    let mut session = Session::open(root.clone(), files, WatchFilter::new(root.to_str().unwrap()));
    session.index_everything();
    let paths: Vec<PathBuf> = session.index().summaries().map(|s| s.path.clone()).collect();
    println!("{} file(s)", paths.len());

    let started = Instant::now();
    let mut tokens_total = 0usize;
    let mut declarations = 0usize;
    let mut bytes = 0usize;
    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        bytes += text.len();
        let (tokens, _) = cpp_parser::lex(&text, &LexerConfig::default());
        tokens_total += tokens.len();

        // **The VS browse parser in miniature**: no tree, no macros, a brace counter and a "a name before
        // `(`/`;`/`{` at depth 0-or-namespace is a declaration" heuristic. The point of the probe is the cost.
        let mut depth = 0i32;
        let mut previous_identifier = false;
        for token in &tokens {
            match token.kind {
                CppTokenKind::LeftBrace => depth += 1,
                CppTokenKind::RightBrace => depth -= 1,
                CppTokenKind::Identifier => previous_identifier = true,
                CppTokenKind::LeftParen | CppTokenKind::Semicolon | CppTokenKind::LeftBrace
                    if previous_identifier && depth <= 1 =>
                {
                    declarations += 1;
                    previous_identifier = false;
                }
                _ => previous_identifier = false,
            }
        }
    }
    println!(
        "token scan: {:?} for {} file(s), {:.1} MB, {tokens_total} token(s), {declarations} declaration-ish",
        started.elapsed(),
        paths.len(),
        bytes as f64 / 1_048_576.0
    );
}
