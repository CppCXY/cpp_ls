// What does `skipped_regions` say for the three `_STL_LANG` defines in `vcruntime.h`?
use cpp_code_analysis::preprocess::preprocess_with_a_seed;

fn main() {
    let path = std::env::args().nth(1).expect("a file");
    let source = std::fs::read_to_string(&path).expect("readable");
    let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
    // **With the seed**, which is the entry point that decides `#ifdef __cplusplus`: the plain `preprocess` has no
    // environment and answers every `#ifdef` false, which inverts every region — a mistake this probe made first.
    let mut seed = cpp_code_analysis::Marked::default();
    seed.define_on_the_command_line("__cplusplus", Some("202400L"));
    seed.define_on_the_command_line("_MSVC_LANG", Some("202400L"));
    let pre = preprocess_with_a_seed(&source, tree.get_tokens(), &seed);

    println!("  {} skipped region(s)", pre.skipped_regions.len());
    for (from, to) in pre.skipped_regions.iter().take(12) {
        let line = source[..*from].matches('\n').count() + 1;
        println!("    {from}..{to}  (line {line})");
    }

    for at in [7116usize, 7236, 7414] {
        let line = source[..at].matches('\n').count() + 1;
        println!("  offset {at} (line {line}) skipped={}", pre.is_skipped(at));
    }
}
