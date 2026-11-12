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
use cpp_parser::{
    CppAstNode, CppDeclaration, CppDeclarator, CppParser, CppSyntaxKind, CppTranslationUnit,
    ParserConfig,
};

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

/// **A parenthesised declarator is a declarator**, not a type-id — the C grammar's `( declarator )`, which neovim
/// writes eight times at file scope and once in a parameter list.
///
/// ```c
/// static char *(names[3]) = { "TMPDIR", … };        // src/nvim/option.c:210 — an array of `char *`
/// static histentry_T *(history[HIST_COUNT]) = …;    // src/nvim/cmdhist.c:35
/// static void createcat (…, int (catf) (int))       // src/lpeg/lptree.c:1308 — a parameter that names one
/// ```
///
/// Every one of them was unreadable. The group holds a name and the declaration continues with an initializer, but
/// the reading that owns a `(` after a type is a *type-id*'s: `(names[3])` became a parameter list — a function
/// returning `char *` that names nothing — so the `=` had no owner and the file reported ``expected `;` ``.
///
/// The name is what the assertion is about, not just the absence of an error: a reading that consumes the tokens
/// but binds `names` as a *parameter* of an unnamed function is the same defect with the diagnostic removed.
#[test]
fn a_parenthesised_declarator_names_what_it_holds() {
    let declarations = [
        ("a keyword type", "static char *(lines[]) = { 0 };", "lines"),
        ("a keyword type, sized", "static char *(names[3]) = { 0 };", "names"),
        (
            "a named type",
            "typedef int T; static T *(lines[3]) = { 0 };",
            "lines",
        ),
        (
            "an elaborated type",
            "struct S { int a; }; static struct S *(lines[2]) = { 0 };",
            "lines",
        ),
        (
            "a qualified type",
            "typedef struct { int a; } S; static S *const (lines[2]) = { 0 };",
            "lines",
        ),
        (
            "a type from a header",
            "static histentry_T *(history[HIST_COUNT]) = { 0 };",
            "history",
        ),
        ("no storage class", "char *(lines[]) = { 0 };", "lines"),
        (
            "a pointer with no suffix",
            "static char *(line) = 0;",
            "line",
        ),
    ];
    for (what, source, declared) in declarations {
        let tree = CppParser::parse(source, ParserConfig::default().with_language(c11()));
        assert_eq!(
            tree.get_errors(),
            [],
            "{what} does not parse: {source} — {:?}",
            tree.get_errors()
        );
        let root = CppTranslationUnit::cast(tree.get_red_root()).expect("a translation unit");
        let name = root
            .get_declarations()
            .last()
            .and_then(|declaration: CppDeclaration| declaration.get_name_text());
        assert_eq!(
            name.as_deref(),
            Some(declared),
            "{what} declares the wrong thing: {source}"
        );
    }
}

/// **A parameter is named by a parenthesised declarator too** — `int (catf) (int)` is one parameter whose name is
/// `catf`, not a type-id followed by a second type.
///
/// Measured on the shape rather than the token count: the declaration has to reach its body, and the parameter has
/// to own a name. Before the rule, `void f(int (catf) (int)) { }` was an expression statement — the whole
/// declaration was refused, so `f` had no parameters and no body.
#[test]
fn a_parenthesised_parameter_names_what_it_holds() {
    let source = "static void createcat (int (catf) (int)) { }";
    let tree = CppParser::parse(source, ParserConfig::default().with_language(c11()));
    assert_eq!(tree.get_errors(), [], "{source} does not parse");
    let root = CppTranslationUnit::cast(tree.get_red_root()).expect("a translation unit");
    let declaration = root
        .get_declarations()
        .next()
        .expect("the function is a declaration");
    assert_eq!(declaration.get_name_text().as_deref(), Some("createcat"));

    let mut names: Vec<String> = declaration
        .syntax()
        .descendants()
        .filter(|node| node.kind() == cpp_parser::CppKind::Syntax(CppSyntaxKind::Parameter))
        .flat_map(|parameter| parameter.descendants().filter_map(CppDeclarator::cast))
        .filter_map(|declarator| declarator.get_name_text())
        .collect();
    // One name per declarator, and a parenthesised one answers at two levels of the same nest — so the question
    // is which *names* the parameters have, not how many nodes carry them.
    names.sort();
    names.dedup();
    assert_eq!(names, ["catf"], "the parameter is named by the group");
}

/// The readings around the shape, which must keep what they had: a product is not a declarator, and the type has
/// to be one for the declaration reading to be licensed.
#[test]
fn a_product_is_not_a_parenthesised_declarator() {
    // `a` is a variable, so `a * (b);` is a multiplication whose result is discarded — not a declaration of `b`.
    let source = "int b; void f(int a) { a * (b); }";
    let tree = CppParser::parse(source, ParserConfig::default().with_language(c11()));
    assert_eq!(tree.get_errors(), [], "{source} does not parse");
    let root = CppTranslationUnit::cast(tree.get_red_root()).expect("a translation unit");
    let declarations: Vec<String> = root
        .syntax()
        .descendants()
        .filter_map(CppDeclaration::cast)
        .filter_map(|declaration| declaration.get_name_text())
        .collect();
    assert_eq!(
        declarations,
        ["b", "f"],
        "`b` must not be declared a second time inside `f`"
    );

    // **An assignment through a pointer is not a declaration either**, and it is the shape the `=` case would
    // otherwise claim: `*(p) = q;` has the group, the operator and the `=`, and no type position at all.
    let source = "void f(int *p, int q) { *(p) = q; }";
    let tree = CppParser::parse(source, ParserConfig::default().with_language(c11()));
    assert_eq!(tree.get_errors(), [], "{source} does not parse");
    let root = CppTranslationUnit::cast(tree.get_red_root()).expect("a translation unit");
    let declared: Vec<String> = root
        .syntax()
        .descendants()
        .filter_map(CppDeclaration::cast)
        .filter_map(|declaration| declaration.get_name_text())
        .collect();
    assert_eq!(declared, ["f"], "{source} declares nothing but the function");
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
