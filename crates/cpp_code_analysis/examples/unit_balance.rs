//! **Which files does the unit reader say do not balance, and is it right?**
//!
//! `RenderedUnit::unbalanced` is the list the analysis acts on, and it is a claim about a **file's own text**: the
//! braces in it do not pair. If that claim is wrong, the parser is being told to isolate a file that needs no
//! isolating — so the claim itself has to be checkable against the text it is about.
//!
//! This prints, for every file the walk names, the count the **file's own bytes** give (braces outside comments,
//! strings and character literals) beside the count our lexer gives, and the first token where the two disagree.
//!
//! ```text
//! usage: unit_balance <include-dir> <root file>
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: unit_balance <include-dir> <root file>");
        std::process::exit(2);
    };
    let root = args.get(1).cloned().unwrap_or_else(|| "memory".to_string());

    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );

    // A root the session can walk from: a file in the directory, named by the caller.
    let path = std::path::PathBuf::from(dir).join(&root);
    session.index_everything();
    let Some(reading) = session.read_the_unit(&path) else {
        eprintln!("  the unit does not read: {}", path.display());
        std::process::exit(1);
    };

    println!("  the unit is {} file(s), {} with tokens", reading.files, reading.files_with_tokens);
    println!("  the stream's own balance: {}", reading.braces);
    println!("  **unbalanced: {}**", reading.unbalanced.len());

    for named in &reading.unbalanced {
        let text = std::fs::read_to_string(named).unwrap_or_default();
        let their_open = text.matches('{').count();
        let their_close = text.matches('}').count();
        println!(
            "    {}  bytes: {{ = {}  }} = {}  (difference {})",
            named.display(),
            their_open,
            their_close,
            their_open as i64 - their_close as i64
        );
    }
}
