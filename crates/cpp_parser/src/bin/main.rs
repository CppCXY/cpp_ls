use cpp_parser::*;

// 读取输入文件名, 然后dump他的语法树或者报错
//
// `--tree` 让"有错也把树打出来"。排查缺规则时最需要的正是这个: 报错只说"哪里不对", 而问题几乎总是
// "它把这个构造读成了什么节点"——树的形状才是判据 (docs/grammar-gaps.md 维护约定第 13 条)。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let filename = args.iter().skip(1).find(|arg| !arg.starts_with("--"));
    let Some(filename) = filename else {
        eprintln!("Usage: cpp_dump <filename> [--tree]");
        return;
    };
    let with_tree = args.iter().any(|arg| arg == "--tree");

    let content = std::fs::read_to_string(filename).expect("Failed to read the file");
    let tree = CppParser::parse(&content, ParserConfig::default());
    let errors = tree.get_errors();
    if !errors.is_empty() {
        let line_index = LineIndex::parse(&content);
        eprintln!("Errors found while parsing the file:");
        for error in errors {
            let (line, col) = line_index
                .get_line_col(error.range.start(), &content)
                .unwrap();
            eprintln!(
                "Error at line {}, column {}: {:?}",
                line, col, error.message
            );
        }
    }

    if errors.is_empty() || with_tree {
        println!("{}", tree.get_unit().dump());
    }
}
