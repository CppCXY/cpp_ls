//! Print the parser's raw event stream, for debugging a shape that came out wrong.
//!
//! ```text
//! cargo run -p cpp_parser --example events -- --source 'Widget w(1);'
//! ```
//!
//! The tree is a fold of the event stream, so when a node ends up in the wrong place the events say
//! which `NodeStart` swallowed it. `examples/dump.rs` shows the tree; this shows why.

use cpp_parser::{CppParser, MarkEvent, ParserConfig};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let source = match args.as_slice() {
        [flag, text, ..] if flag == "--source" => text.clone(),
        [path, ..] => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("cannot read {path}: {err}");
                return;
            }
        },
        [] => {
            eprintln!("usage: events --source '<code>'");
            return;
        }
    };

    let (tree, events) = CppParser::parse_with_events(&source, ParserConfig::default());
    let text = tree.to_source_text();

    for (index, event) in events.iter().enumerate() {
        match event {
            MarkEvent::NodeStart { kind, .. } => println!("{index:>4}  start {kind:?}"),
            MarkEvent::NodeEnd => println!("{index:>4}  end"),
            MarkEvent::Trivia => println!("{index:>4}  trivia"),
            MarkEvent::EatToken { kind, range } => {
                let slice = &text[range.start_offset..range.end_offset()];
                println!("{index:>4}  token {kind:?} {slice:?}");
            }
        }
    }

    for error in tree.get_errors() {
        println!("error: {}", error.message);
    }
}
