//! **Where the time goes when a file is parsed**: per byte, and at several sizes.
//!
//! ```text
//! cargo run --release -p cpp_parser --example parse_scale -- <file>
//! ```
//!
//! # Why this exists
//!
//! The censuses measure a corpus, and a corpus number cannot say *why*: `std_probe` reports one total for
//! "environment + cook + parse" per file, and 255 files of rendered standard headers were taking 33 s for 3.2 MB
//! while the same corpus's **raw** text parsed at 2 MB/s. A total like that has three possible shapes — the
//! lexer, the tree, or something that grows faster than the input — and only a run at several sizes can tell them
//! apart: a linear reader's cost per byte is flat, and anything above linear shows up as a rising one.
//!
//! So this prints, for each size: bytes, the time to **lex**, the time to **parse**, and each as milliseconds per
//! kilobyte. The prefix sizes are powers of two fractions of the file, which is enough to see a slope and cheap
//! enough to run on a single large header.
//!
//! The rendering is the interesting input because it is **one line** — directives gone, one branch of every
//! conditional kept — and a rule that scans by line, or a structure that grows with the line's length, has
//! nothing to break the work up.

use std::time::Instant;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        println!("usage: parse_scale <file>");
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("{path} could not be read");
        return;
    };

    let lexer_config = cpp_parser::LexerConfig::default();
    let parser_config = cpp_parser::ParserConfig::default().with_lexer_config(lexer_config);

    println!("file {} | {} bytes | {} lines", path, text.len(), text.lines().count());
    println!("{:>10}  {:>9}  {:>9}  {:>9}  {:>9}", "bytes", "lex ms", "parse ms", "lex ms/KB", "parse ms/KB");

    // Fractions of the file, smallest first, plus the whole thing: `/8`, `/4`, `/2`, `1`. A prefix cut in the
    // middle of a token is a lexer error rather than a panic — the lexer is total, and this is a timing run.
    for divisor in [8usize, 4, 2, 1] {
        let size = text.len() / divisor;
        let slice = &text[..floor_to_a_char_boundary(&text, size)];

        let started = Instant::now();
        let (tokens, _) = cpp_parser::lex(slice, &lexer_config);
        let lexed = started.elapsed();

        let started = Instant::now();
        let tree = cpp_parser::CppParser::parse(slice, parser_config_for(&parser_config));
        let parsed = started.elapsed();

        let kilobytes = slice.len() as f64 / 1024.0;
        println!(
            "{:>10}  {:>9.2}  {:>9.2}  {:>9.3}  {:>9.3}   ({} tokens, {} errors)",
            slice.len(),
            lexed.as_secs_f64() * 1000.0,
            parsed.as_secs_f64() * 1000.0,
            lexed.as_secs_f64() * 1000.0 / kilobytes.max(0.001),
            parsed.as_secs_f64() * 1000.0 / kilobytes.max(0.001),
            tokens.len(),
            tree.get_errors().len(),
        );
    }
}

/// The largest char boundary at or below `at`, so a slice never splits a multi-byte character.
fn floor_to_a_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// A fresh parser configuration per run: parsing takes the node cache by `&mut`, and a cached tree would make the
/// second run a different measurement from the first.
fn parser_config_for(config: &cpp_parser::ParserConfig<'_>) -> cpp_parser::ParserConfig<'static> {
    cpp_parser::ParserConfig::default().with_lexer_config(config.lexer_config())
}
