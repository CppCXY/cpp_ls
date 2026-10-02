use cpp_parser::*;

// 读取输入文件名, 然后 dump 他的语法树或者报错
//
// `--tree` 让"有错也把树打出来"。排查缺规则时最需要的正是这个: 报错只说"哪里不对", 而问题几乎总是
// "它把这个构造读成了什么节点"——树的形状才是判据。
//
// **`--body` / `--macro` 已经删掉了**, 删的原因是方向本身: 这个文法读的是**预处理之后**的文本, 宏在它开始
// 之前就被替换掉了, 所以它对"某个名字是不是宏"没有、也不该有意见。那两条通道是给一个会看着宏做读法的解析器
// 用的, 而那个解析器正是要走掉的东西 —— 见 `crate::symbols` 的模块文档和 `CppParser` 上关于
// `greater_than_is_an_operator` 的说明。
//
// 要观察一个真实文件被预处理之后的样子, 用 cpp_code_analysis 的 `CPPLS_DUMP`:
//   CPPLS_DUMP=<out> cargo run -p cpp_code_analysis --example align_preprocessor -- <file> --ours-only
// 然后把 `<out>` 交给这个工具。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let filename = args.iter().skip(1).find(|arg| !arg.starts_with("--"));
    let Some(filename) = filename else {
        eprintln!("Usage: cpp_dump <filename> [--tree] [--errors-only]");
        return;
    };
    let with_tree = args.iter().any(|arg| arg == "--tree");
    // **`--errors-only` exists because the dump is the expensive part, not the parse.** On the 3.3 MB cooked
    // `<vector>` stream the parse is about 0.8 s and `tree.get_unit().dump()` is about 14 s: it builds a
    // 1.2-million-line string and writes it out. A check that wants *the number of diagnostics* — which is most
    // of them — was paying for all of it and throwing the string away.
    let errors_only = args.iter().any(|arg| arg == "--errors-only");

    let content = std::fs::read_to_string(filename).expect("Failed to read the file");
    let tree = CppParser::parse(&content, ParserConfig::default());
    let errors = tree.get_errors();

    if errors_only {
        println!("{filename} errors={}", errors.len());
        return;
    }

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
