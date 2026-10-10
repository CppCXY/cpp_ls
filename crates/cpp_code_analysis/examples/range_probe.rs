use cpp_code_analysis::{MemoryFiles};
fn main() {
    const MAIN: &str = "int main() { int count = \"three\"; return count; }\n";
    let root = cpp_parser::CppParser::parse(MAIN, cpp_parser::ParserConfig::default()).get_red_root();
    for node in root.descendants() {
        let kind = format!("{:?}", cpp_parser::CppSyntaxKind::from(node.kind()));
        if kind.contains("Initializer") || kind.contains("Variable") || kind.contains("Declarator") || kind.contains("Name") {
            let r = cpp_parser::source_range(node.text_range());
            eprintln!("{kind:<28} {}..{} {:?}", r.start_offset, r.end_offset(), &MAIN[r.start_offset..r.end_offset().min(MAIN.len())]);
        }
    }
    let _ = MemoryFiles::new();
}
