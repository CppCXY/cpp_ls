//! End-to-end smoke test: the public entry point really does produce a tree.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

#[test]
fn parses_a_file_end_to_end() {
    let source = concat!(
        "#include <iostream>\n",
        "\n",
        "class Greeter {\n",
        "public:\n",
        "    void greet() const;\n",
        "};\n",
        "\n",
        "int main() {\n",
        "    Greeter g;  // construct\n",
        "    g.greet();\n",
        "    return 0;\n",
        "}\n",
    );

    let tree = CppParser::parse(source, ParserConfig::default());

    assert_eq!(tree.root_kind(), CppSyntaxKind::TranslationUnit);
    assert_eq!(tree.to_source_text(), source);
    assert_eq!(tree.text_len(), source.len());

    // The public entry point must never fail, and must never invent tokens.
    let root = tree.get_red_root();
    assert_eq!(usize::from(root.text_range().len()), source.len());
}
