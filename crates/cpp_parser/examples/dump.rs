//! Print the syntax tree of a file, for looking at what the parser actually built.
//!
//! ```text
//! cargo run -p cpp_parser --example dump -- path/to/file.cpp
//! cargo run -p cpp_parser --example dump -- --source 'Widget w(1);'
//! ```
//!
//! A debugging tool rather than a test: a failing assertion says *that* the shape is wrong, and this
//! says *what* the shape is. It is the quickest way to answer "which node did that end up in?", which
//! a test's error message rarely shows.

use std::process::ExitCode;

use cpp_parser::{CppParser, CppSyntaxTree, ParserConfig};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let (label, source) = match args.as_slice() {
        [flag, text, ..] if flag == "--source" => ("<argument>".to_string(), text.clone()),
        [path, ..] => match std::fs::read_to_string(path) {
            Ok(text) => (path.clone(), text),
            Err(err) => {
                eprintln!("cannot read {path}: {err}");
                return ExitCode::from(2);
            }
        },
        [] => {
            eprintln!("usage: dump <file.cpp> | dump --source '<code>'");
            return ExitCode::from(2);
        }
    };

    let tree = CppParser::parse(&source, ParserConfig::default());
    report(&label, &source, &tree);

    if tree.has_syntax_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report(label: &str, source: &str, tree: &CppSyntaxTree) {
    println!("=== {label} ({} bytes) ===", source.len());
    println!("{:#?}", tree.get_red_root());

    let errors = tree.get_errors();
    if errors.is_empty() {
        println!("--- no errors ---");
        return;
    }

    println!("--- {} error(s) ---", errors.len());
    for error in errors {
        let range = error.range;
        println!(
            "{:?} at {}..{}: {}",
            error.kind,
            u32::from(range.start()),
            u32::from(range.end()),
            error.message
        );
    }
}
