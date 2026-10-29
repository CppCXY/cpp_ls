//! **One real file's top-level structure** — the probe for the parser's silent failures.
//!
//! ```text
//! $env:CPPLS_FILE = "E:\neovim\src\nvim\ex_cmds_defs.h"
//! cargo test --release -p cpp_parser --test parse_probe -- --ignored --nocapture
//! ```
//!
//! # Why a *structure* dump rather than an error list
//!
//! The parser is total: a construct it cannot read is recovered from, and recovery is usually silent. The worst
//! shape is a **declaration that never ends** — it takes every declaration after it into its own node, so the file
//! parses without an error, the tree is lossless, and everything below the failure simply is not there. Measured on
//! neovim's `ex_cmds_defs.h`: `typedef char *(*LineGetter)(int, void *, int, bool);` came out as one `TypedefDecl`
//! spanning to the end of the header — 16 facts instead of 93, no `struct exarg`, and every `exarg_T *` member
//! question in the project answered nothing.
//!
//! An error list says nothing about that file. **A declaration whose token range is a large fraction of the
//! file** does, and so does the line each top-level child starts on: a jump of hundreds of lines between two
//! siblings is the signature. Both are printed here, next to the error nodes, so that one run answers "did this
//! file read, and if not, where did it stop reading".
//!
//! The probe takes its file from `CPPLS_FILE` and **skips** when that is absent — a probe that needs one machine's
//! checkout must not fail on the next machine (see `docs/remaining.md` §C1).

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

#[test]
#[ignore = "reads a file named by CPPLS_FILE; run with --ignored"]
fn how_one_file_parses() {
    let Some(path) = std::env::var_os("CPPLS_FILE").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set CPPLS_FILE");
        return;
    };
    let source = std::fs::read_to_string(&path).expect("the file is readable");
    let tree = CppParser::parse(&source, ParserConfig::default());
    let root = tree.get_red_root();
    let total = source.len().max(1);

    let line_of = |offset: usize| source[..offset.min(source.len())].matches('\n').count() + 1;
    let errors: Vec<String> = root
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode)
        .map(|node| {
            let text = node.text().to_string().replace('\n', " ");
            let text: String = text.chars().take(60).collect();
            format!(
                "line {}: {text:?}",
                line_of(usize::from(node.text_range().start()))
            )
        })
        .collect();

    println!(
        "{} line(s), {} byte(s), {} error node(s)",
        line_of(source.len()),
        source.len(),
        errors.len()
    );
    for error in errors.iter().take(20) {
        println!("  error {error}");
    }

    println!("--- top-level children");
    for child in root.children() {
        let span = child.text_range();
        let share = 100.0 * f64::from(u32::from(span.len())) / total as f64;
        let text = child.text().to_string().replace('\n', " ");
        let text: String = text.chars().take(64).collect();
        println!(
            "  line {:>5}  {:>5.1}% of the file  {:?} {text:?}",
            line_of(usize::from(span.start())),
            share,
            CppSyntaxKind::from(child.kind()),
        );
    }
}
