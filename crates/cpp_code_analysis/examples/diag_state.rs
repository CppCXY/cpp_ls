//! **Where an error found in the rendering comes from, and where it goes.**
//!
//! ```text
//! cargo run --release --example diag_state -p cpp_code_analysis
//! ```
//!
//! We never parse the file's own text, so an error has to be *discovered* in the rendering and *placed* back in the
//! file. This prints both halves for three shapes: an error in the file's own tokens, one inside a macro body, and
//! one a token paste produced. The middle and last are the interesting ones — the text they are about exists in no
//! file in the form the parser rejected.
use cpp_code_analysis::{MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter, CompilerConfig};

fn probe(label: &str, files: &[(&str, &str)], main: &str) {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
    session.did_open("/p/main.cpp", main);
    session.index_everything();

    println!("--- {label} ---");
    println!("  the file writes: {main:?}");
    for (path, text) in files {
        println!("  {path} writes: {text:?}");
    }
    match session.diagnostics(std::path::Path::new("/p/main.cpp")) {
        Some(found) => {
            println!(
                "  published for main.cpp: reading {:?}, {} error(s), {} unplaced",
                found.reading,
                found.errors.len(),
                found.unplaced
            );
            for error in &found.errors {
                println!("      at {}..{} (a file offset): {}", error.start, error.end, error.message);
                println!(
                    "      the text there: {:?}",
                    &main[error.start.min(main.len())..error.end.min(main.len())]
                );
            }
        }
        None => println!("  published: NONE"),
    }
}

fn main() {
    probe(
        "an error in the file's own tokens",
        &[],
        "struct S { int a }\n",
    );
    probe(
        "an error inside a macro body",
        &[("/p/bad.h", "#define DECLARE struct S { int a }\n")],
        "#include \"bad.h\"\nDECLARE;\n",
    );
    probe(
        "an error a token paste produces",
        &[("/p/paste.h", "#define CAT(a, b) a##b\n")],
        "#include \"paste.h\"\nstruct S { int CAT(x, ;) };\n",
    );
}
