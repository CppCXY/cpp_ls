//! **Does a `namespace` that a file leaves open hold the next file?** — a probe, not a test.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test scope_probe -- --ignored --nocapture
//! ```
//!
//! The modules fixture answers `std::std::std::std::std::std::std::std::std::std::basic_format_arg::handle`
//! where the scope should be `std`, and the parser is where that has to come from. This isolates it: two "files"
//! in one text, the first opening a `namespace std {` it never closes, a boundary between them, and one declaration
//! in each. What the second declaration's ancestors are is the whole question.

use cpp_parser::{CppParser, ParserConfig};

#[test]
#[ignore = "a probe: prints a parse tree, asserts nothing"]
fn a_namespace_that_one_file_leaves_open() {
    // File 1 opens `namespace std {` and never closes it; file 2 declares something.
    let first = "namespace std {\nint a;\n";
    let second = "int b;\n";
    let text = format!("{first}{second}");

    let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
    // **The boundary is a token offset** — the first token that belongs to the second file. See
    // `ParserConfig::with_file_boundaries`, which takes offsets into the token list and not bytes.
    // **The boundary is a BYTE offset** — `crossed_a_file_boundary_since` compares it against
    // `current_token_range().start_offset`, which is bytes. `RenderedUnit::file_boundaries` returns cooked byte
    // offsets for the same reason. A token index here is silently never crossed.
    let boundary = tokens
        .iter()
        .find(|token| token.range.start_offset >= first.len())
        .map(|token| token.range.start_offset)
        .expect("the second file has a token");
    println!("boundary byte {boundary} of {} byte(s)", text.len());

    let config = ParserConfig::default().with_file_boundaries(vec![boundary]);
    let tree = CppParser::parse(&text, config);

    fn print(tree: &cpp_parser::CppSyntaxNode, text: &str, depth: usize) {
        let kind = format!("{:?}", tree.kind());
        let slice = &text[tree.text_range().start().into()..tree.text_range().end().into()];
        let sample: String = slice
            .chars()
            .take(40)
            .collect::<String>()
            .replace('\n', " ");
        println!("{:indent$}{kind} {sample:?}", "", indent = depth * 2);
        for child in tree.children() {
            print(&child, text, depth + 1);
        }
    }
    let red = tree.get_red_root();
    print(&red, &text, 0);

    // And the question this exists for, in one line: is `b` inside a namespace?
    let mut inside: Vec<String> = Vec::new();
    let mut node = red.descendants().find(|node| {
        &text[usize::from(node.text_range().start())..usize::from(node.text_range().end())] == "b"
    });
    while let Some(current) = node {
        let kind = format!("{:?}", current.kind());
        if kind.contains("Namespace") || kind.contains("TranslationUnit") {
            inside.push(kind);
        }
        node = current.parent();
    }
    inside.reverse();
    println!("ancestors of `b`: {inside:?}");
}
