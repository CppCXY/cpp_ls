//! **A C file is a C file** — the keyword table the language decides.
//!
//! C++ reserves `new`, `delete`, `this`, `operator`, `template`, `class`, and the alternative spellings (`compl`,
//! `not`, `and`, …). C reserves none of them, and a C program is entitled to use any of them as a name — measured on
//! neovim, which has `Object new = …` (`src/nvim/autocmd.c:1343`), `bool delete` in a parameter list
//! (`src/nvim/marktree.c:392`) and a completion list called `compl` (`src/nvim/insexpand.c:4116`). Read with C++'s
//! table, the first two make their **declaration** unparsable, the expression reading takes the `{`, and a whole
//! function body becomes one expression statement.
//!
//! The language comes from the compile database (`-std=gnu11`) or, when there is none, from the extension — see
//! `CompilerConfig::language_for`.

use cpp_parser::features::{LanguageConfig, Standard};
use cpp_parser::{CppParser, ParserConfig};

fn errors_in(source: &str, language: LanguageConfig) -> Vec<String> {
    let tree = CppParser::parse(source, ParserConfig::default().with_language(language));
    tree.get_errors()
        .iter()
        .map(|error| format!("{error:?}"))
        .collect()
}

fn c11() -> LanguageConfig {
    LanguageConfig::new().with_standard(Standard::C11)
}

fn cpp23() -> LanguageConfig {
    LanguageConfig::new().with_standard(Standard::Cpp23)
}

#[test]
fn names_cpp_reserves_are_names_in_c() {
    let sources = [
        (
            "a variable called new",
            "struct T { int x; }; struct T *new = 0; void g(void) { new->x = 1; }",
        ),
        (
            "an object called new",
            "struct O { int x; }; struct O new = { 0 };",
        ),
        (
            "a parameter called delete",
            "void f(int delete) { (void)delete; }",
        ),
        (
            "a variable called compl",
            "int compl = 0; int g(void) { compl = compl + 1; return compl; }",
        ),
        (
            "a variable called this",
            "int this = 0; int g(void) { return this; }",
        ),
        ("a variable called operator", "int operator = 0;"),
        ("a variable called template", "int template = 0;"),
        ("a variable called class", "int class = 0;"),
    ];
    for (name, source) in sources {
        let errors = errors_in(source, c11());
        assert!(errors.is_empty(), "{name} does not parse in C: {errors:?}");
    }
}

#[test]
fn the_same_names_are_keywords_in_cpp() {
    // `new` and `delete` in a declarator position are the C++ keywords, and the declaration is not one.
    assert!(!errors_in("int new = 0;", cpp23()).is_empty());
    assert!(!errors_in("void f(int delete);", cpp23()).is_empty());
    // …while a `new` expression is C++ syntax and parses as one.
    let errors = errors_in("void f() { int *p = new int(1); (void)p; }", cpp23());
    assert!(errors.is_empty(), "{errors:?}");
}
