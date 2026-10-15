//! **The two readings of a file, and which one a check is handed.**
//!
//! This crate keeps two of them, and the difference decides what a semantic check can even see:
//!
//! ```text
//!   Session::view(path)          the file's OWN view — its own tokens, no macros expanded, no headers inlined
//!   Session::rendering_of(path)  the rendering, with the spans that map it back — what a compiler's parser reads
//! ```
//!
//! [`Session::diagnostics`](cpp_code_analysis::Session::diagnostics) hands the checks the **first** of the two. So a
// check sees this file and nothing else: a class, a function or a macro from a header is never a fact it can report
// on, which is the scope the checks are meant to have.
//!
//! This prints both, so the difference is a number rather than a claim:
//!
//! ```text
//!   usage: check_walk <directory> <file>
//! ```
//!
//! The include has to resolve for the second number to be interesting — pass a directory the file's `#include`s are
//! found in. **A file whose includes do not resolve gives two equal numbers**, which is a fixture measuring nothing:
//! this workspace has fallen into that three times (see `tests/checks.rs` on angle includes, and on the pump → ask →
//! pump contract that has to be honoured before a rendering exists at all).

use cpp_code_analysis::{
    CompilerConfig, DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(file)) = (args.next(), args.next()) else {
        eprintln!("usage: check_walk <directory> <file>");
        return;
    };
    let root = std::path::PathBuf::from(&dir);
    let path = std::path::PathBuf::from(&file);

    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default().with_include_path(&root),
    );
    session.add_project_files([path.clone()]);
    session.did_open(&path, &std::fs::read_to_string(&path).unwrap_or_default());

    // **The pump, the ask, the pump** — the contract the whole workspace states once, in `tests/completion.rs`. A
    // rendering is built because something asked for one, and the ask answers `None` the first time on purpose.
    session.index_everything();
    let _ = session.view(&path);
    session.index_everything();

    let source = std::fs::read_to_string(&path).unwrap_or_default();
    println!("file                  {file}  ({} bytes)", source.len());

    match session.view(&path) {
        Some(view) => {
            let nodes = view.tree.get_red_root().descendants().count();
            let written = view
                .written_text()
                .map(str::len)
                .unwrap_or(view.source.len());
            println!();
            println!("what a CHECK is handed - Session::view");
            println!("  text                {} bytes", view.source.len());
            println!("  nodes               {nodes}");
            println!("  the file's own text {written} bytes (the same text, today)");
        }
        None => println!("no view for {file}: is it in the project?"),
    }

    match session.rendered_text_of(&path) {
        Some(rendered) => {
            println!();
            println!("what a COMPILER's parser would read - Session::rendering_of");
            println!("  text                {} bytes", rendered.len());
            println!(
                "  ratio               {}x the file",
                rendered.len() / source.len().max(1)
            );
            println!(
                "  (not parsed here: a header's rendering is megabytes, and parsing it to count nodes is the"
            );
            println!("   cost this example exists to keep out of the check path)");
        }
        None => println!("no rendering for {file}: its closure has not been read"),
    }
}
