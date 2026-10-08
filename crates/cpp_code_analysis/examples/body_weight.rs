//! **How much of a header is function bodies** — the ceiling for `docs/indexing-performance.md` item 4.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example body_weight -- <dir>
//! ```
//!
//! Item 4 is "do not build subtrees for bodies while indexing", which all three reference tools do (clangd's
//! `SkipFunctionBodies`, IntelliJ's `skipChildProcessingWhenBuildingStubs`, Visual Studio's *"it skips the content of
//! blocks"*). Before writing a parser mode for it, the question is **what fraction of the work it removes** — and that
//! is a question about the shape of real headers, not about the parser.
//!
//! It counts, over the closure of a real translation unit:
//!
//! ```text
//!   nodes                    what `descendants()` walks and the sweep pays for
//!   inside a CompoundStat   the part item 4 would stop building
//!   tokens                   what the lexer produces, which item 4 does NOT remove
//! ```
//!
//! **Tokens are the point of the second column.** A rowan tree is lossless: the body's tokens must stay in it even if
//! nothing is built on top of them, so the ceiling for item 4 is the *node* count and not the token count. If the two
//! were the same number the change would be worth nothing.

use std::path::{Path, PathBuf};

use cpp_parser::CppSyntaxKind;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: body_weight <dir>");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = home.join("main.cpp");
    let text = std::fs::read_to_string(&entry).expect("the entry file reads");

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    session.did_open(&entry, &text);
    session.index_everything();

    let mut files = 0usize;
    let mut nodes = 0usize;
    let mut nodes_in_bodies = 0usize;
    let mut tokens = 0usize;
    let mut tokens_in_bodies = 0usize;

    for summary in session.index().summaries() {
        let Ok(text) = std::fs::read_to_string(&summary.path) else {
            continue;
        };
        files += 1;
        let (lexed, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();

        let in_a_body = |node: &cpp_parser::CppSyntaxNode| {
            node.ancestors()
                .any(|above| CppSyntaxKind::from(above.kind()) == CppSyntaxKind::CompoundStat)
        };

        for node in root.descendants() {
            nodes += 1;
            if in_a_body(&node) {
                nodes_in_bodies += 1;
            }
        }
        // Tokens are the leaves, and a lossless tree keeps them whether or not anything is built over them — which is
        // why this column exists: it is what item 4 *cannot* remove. Counted by the token's own parent, the same way
        // the nodes are: summing each block's own `descendants_with_tokens` counts a nested block once per enclosing
        // block, which reported 98.6% the first time this ran. That was a bug in the probe, not a finding.
        for element in root.descendants_with_tokens() {
            if element.as_token().is_some() {
                tokens += 1;
                if element.parent().is_some_and(|parent| in_a_body(&parent)) {
                    tokens_in_bodies += 1;
                }
            }
        }
        let _ = lexed;
    }

    let share = |part: usize, whole: usize| {
        if whole == 0 {
            0.0
        } else {
            part as f64 * 100.0 / whole as f64
        }
    };

    println!("{files} file(s)\n");
    println!("  nodes  {nodes:>9}   in a body {nodes_in_bodies:>9}   {:>5.1}%", share(nodes_in_bodies, nodes));
    println!("  tokens {tokens:>9}   in a body {tokens_in_bodies:>9}   {:>5.1}%", share(tokens_in_bodies, tokens));
    println!(
        "\n  item 4's ceiling: the node `descendants()` walk shrinks by {:.1}%, and the lexer's work does not shrink",
        share(nodes_in_bodies, nodes)
    );
    let _ = Path::new("");
}
