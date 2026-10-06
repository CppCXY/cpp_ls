//! What does the grammar make of this text? **A tree dump**, for the questions a `match` cannot answer.
//!
//! Written for `new int` and `new Box<int>()` parsing differently: the first has no `TypeId` child and the second
//! does, and no amount of reading the expression reader says which node a *primitive* type lands in. The kinds are
//! the parser's own, printed with the text of each node cut short, so a shape is visible at a glance.
//!
//! ```text
//! usage: dump_tree <file> [<node-kind-to-focus-on>]
//! ```
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dump_tree <file> [<kind>]");
        std::process::exit(2);
    };
    let focus = args.next();

    let source = std::fs::read_to_string(&path).expect("the file is readable");
    let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());

    let root = tree.get_red_root();

    fn walk(node: &cpp_parser::CppSyntaxNode, depth: usize, focus: &Option<String>) {
        let kind = format!("{:?}", cpp_parser::CppSyntaxKind::from(node.kind()));

        if let Some(wanted) = focus
            && !kind.contains(wanted.as_str())
        {
            for element in node.children_with_tokens() {
                if let cpp_parser::CppSyntaxElement::Node(child) = element {
                    walk(&child, depth, focus);
                }
            }
            return;
        }

        let text = node.text().to_string();
        let text = text.trim().replace(['\n', '\r'], " ");
        println!(
            "{:indent$}{kind}  {}",
            "",
            &text[..text.len().min(60)],
            indent = depth * 2
        );

        // **Under a matching node the whole subtree is printed, focus or not.** The first version kept filtering
        // below the match, so `NewExpr` printed its own one token and stopped — and the type child, which is the
        // entire question, was filtered out as "not a NewExpr". A focus chooses *where to start*, not what to keep.
        for element in node.children_with_tokens() {
            match element {
                cpp_parser::CppSyntaxElement::Node(child) => walk(&child, depth + 1, &None),
                cpp_parser::CppSyntaxElement::Token(token) => {
                    let token_kind = format!("{:?}", token.kind());
                    if token_kind.contains("Whitespace") || token_kind.contains("Newline") {
                        continue;
                    }
                    println!(
                        "{:indent$}· {token_kind} {:?}",
                        "",
                        token.text(),
                        indent = (depth + 1) * 2
                    );
                }
            }
        }
    }

    walk(&root, 0, &focus);

}
