use cpp_parser::*;

// 读取输入文件名, 然后dump他的语法树或者报错
//
// `--tree` 让"有错也把树打出来"。排查缺规则时最需要的正是这个: 报错只说"哪里不对", 而问题几乎总是
// "它把这个构造读成了什么节点"——树的形状才是判据 (docs/grammar-gaps.md 维护约定第 13 条)。
//
// `--body` / `--macro` 把**宏的体**喂进来, 这是同一个文件在"有人告诉它这个宏是什么"之后的样子。两条通道
// 分开给, 因为它们在这条代码里是分开的, 而且混起来量过: 把条件性的定义当定义发出去会让规则因为"有人认识
// 这个名字"而关掉读形状的路 (`cpp_parser::MacroEnvironment` 的字段注释有那次测量), 而**体**只能打开读法。
//
//   --body  NAME=TEXT    体在"生效的那一支"里 —— 包含链带进来的条件性定义走这条
//   --macro NAME=TEXT    定义本身在生效的位置上, 体一起给 —— 文件自己 #define 的和无条件的走这条
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let filename = args.iter().skip(1).find(|arg| !arg.starts_with("--"));
    let Some(filename) = filename else {
        eprintln!("Usage: cpp_dump <filename> [--tree] [--body NAME=TEXT] [--macro NAME=TEXT]");
        return;
    };
    let with_tree = args.iter().any(|arg| arg == "--tree");

    let mut seeds = Vec::new();
    let mut bodies: Vec<(Box<str>, Box<str>)> = Vec::new();
    let mut definitions: Vec<(Box<str>, Box<str>)> = Vec::new();
    let mut index = 1usize;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag == "--body" || flag == "--macro" {
            let Some(spec) = args.get(index + 1) else {
                eprintln!("{flag} needs NAME=TEXT");
                return;
            };
            let Some((name, text)) = spec.split_once('=') else {
                eprintln!("{flag} needs NAME=TEXT, got `{spec}`");
                return;
            };
            if flag == "--body" {
                bodies.push((name.into(), text.into()));
            } else {
                definitions.push((name.into(), text.into()));
            }
            index += 2;
            continue;
        }
        index += 1;
    }

    for (name, text) in &definitions {
        seeds.push(IncludedMacro::defined_with_body(
            0,
            name,
            false,
            MacroBody::Unknown,
            Some(text),
        ));
    }

    let environment = MacroEnvironment::from_included_macros(seeds).with_bodies_in_force(bodies);
    let config = if environment.is_empty() {
        ParserConfig::default()
    } else {
        ParserConfig::default().with_macros_from_includes(&environment)
    };

    let content = std::fs::read_to_string(filename).expect("Failed to read the file");
    let tree = CppParser::parse(&content, config);
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
