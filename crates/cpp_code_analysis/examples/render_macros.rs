//! **What does a rendering's macro table hold?** The question that decides whether a cooked reading can answer
//! "what is a name defined as here" without re-deciding any region.
//!
//! A rendering is the preprocessed token stream: macros are expanded, so a `#define` line has been *consumed* rather
//! than written out. If the rendering keeps no macro facts at all, then it cannot be the source of a macro
//! environment — and the raw reading, with all its guarded `#define`s, is the only thing left.
//!
//! ```text
//! usage: render_macros <file>
//! ```
fn main() {
    let path = std::env::args().nth(1).expect("a file");
    let source = std::fs::read_to_string(&path).expect("readable");

    let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
    let rendered = cpp_code_analysis::preprocess::cooked::cook(&source, &tokens).render();

    println!("  the file is {} bytes, the rendering {} bytes", source.len(), rendered.text.len());

    let (rendered_tokens, _) = cpp_parser::lex(&rendered.text, &cpp_parser::LexerConfig::default());
    let pre = cpp_code_analysis::preprocess::preprocess(&rendered.text, &rendered_tokens);

    println!("  the rendering has {} directive(s)", pre.directives.len());
    println!("  the rendering's macro table holds {} binding(s)", pre.macros.iter().count());

    for definition in pre.macros.iter().take(8) {
        println!("    {}", definition.name);
    }

    // What the file itself says, for the comparison.
    let own = cpp_code_analysis::preprocess::preprocess(&source, &tokens);
    println!("  the file itself holds {} binding(s)", own.macros.iter().count());
}
