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

    // Depth is tracked so that a stream whose nesting is wrong can be read by eye: the number is how
    // many nodes are open *before* this event.
    let mut depth = 0isize;
    for (index, event) in events.iter().enumerate() {
        match event {
            MarkEvent::NodeStart { kind, .. } => {
                println!("{index:>4} {depth:>3}  start {kind:?}");
                depth += 1;
            }
            MarkEvent::NodeEnd => {
                depth -= 1;
                println!("{index:>4} {depth:>3}  end");
            }
            MarkEvent::Trivia => println!("{index:>4} {depth:>3}  trivia"),
            MarkEvent::EatToken { kind, range } => {
                let slice = &text[range.start_offset..range.end_offset()];
                println!("{index:>4} {depth:>3}  token {kind:?} {slice:?}");
            }
        }
    }

    for error in tree.get_errors() {
        println!("error: {}", error.message);
    }

    if std::env::var("CPP_EVENTS_STACK").is_ok() {
        // Replay the stream against a stack and report the open nodes at each export token.
        let mut stack: Vec<(String, usize)> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            match event {
                MarkEvent::NodeStart { kind, .. } => {
                    stack.push((format!("{kind:?}"), index));
                }
                MarkEvent::NodeEnd => {
                    let closed = stack.pop();
                    if closed.is_none() {
                        println!("event {index}: NodeEnd with nothing open");
                    }
                }
                MarkEvent::EatToken { kind, range } if *kind == cpp_parser::CppTokenKind::ExportKeyword => {
                    println!(
                        "export at {}: open = {:?}",
                        range.start_offset,
                        stack
                            .iter()
                            .map(|(kind, at)| format!("{kind}@{at}"))
                            .collect::<Vec<_>>()
                    );
                }
                _ => {}
            }
        }
    }
}
