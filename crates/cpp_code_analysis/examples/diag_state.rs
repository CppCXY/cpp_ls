use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;
fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("dir"));
    let file = PathBuf::from(std::env::args().nth(2).expect("file"));
    let mut session = Session::open(root.clone(), SessionFiles::new(OpenDocuments::new(), DiskFiles), WatchFilter::new(&root));
    session.index_everything();
    let Some(source) = std::fs::read_to_string(&file).ok() else { panic!("unreadable") };
    session.did_open(&file, &source);
    session.index_everything();

    println!("--- what a client would be shown for {} ---", file.file_name().unwrap_or_default().to_string_lossy());
    match session.diagnostics(&file) {
        Some(found) => {
            println!("  diagnostics: reading {:?} | {} error(s), {} note(s), {} check(s), {} unplaced",
                found.reading, found.errors.len(), found.notes.len(), found.checks.len(), found.unplaced);
            for error in found.errors.iter().take(4) {
                println!("      {}..{} {}", error.start, error.end, error.message);
            }
        }
        None => println!("  session.diagnostics answered NONE -- the client is shown an empty list"),
    }
    // And the raw parse's own errors, which is what we would be hiding.
    let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
    let raw_errors = tree.get_errors().len();
    println!("  the file's own text has {raw_errors} parse error(s) that a raw reading would have reported");
}
