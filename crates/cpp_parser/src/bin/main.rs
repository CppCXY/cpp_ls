use cpp_parser::*;

// 读取输入文件名, 然后dump他的语法树或者报错
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cpp_dump <filename>");
        return;
    }
    let filename = &args[1];
    let content = std::fs::read_to_string(filename).expect("Failed to read the file");
    let tree = CppParser::parse(&content, ParserConfig::default());
    let errors = tree.get_errors();
    if !errors.is_empty() {
        let line_index = LineIndex::parse(&content);
        eprintln!("Errors found while parsing the file:");
        for error in errors {
            let (line, col) = line_index.get_line_col(error.range.start(), &content).unwrap();
            eprintln!("Error at line {}, column {}: {:?}", line, col, error.message);
        }
    } else {
        println!("{}", tree.get_unit().dump());
    }
}
