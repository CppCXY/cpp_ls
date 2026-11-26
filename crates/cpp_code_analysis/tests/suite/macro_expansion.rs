//! **A macro's expansion, as the hover shows it**: the substitution a compiler makes — `#` and `##` applied, and
//! every macro named in the result expanded again — and then formatted.
//!
//! The hover used to show a *textual* substitution: the body with the arguments put in, `##` left as two hashes in
//! the text, and the result's own macro names untouched. Measured on a real project, `kvi_ensure_space`'s card showed
//! `kvi_resize((v), (v).capacity)` for an argument that was itself a macro — a reader comparing it with a compiler's
//! answer was looking at two different things.

use cpp_code_analysis::{
    CompileCommand, CompileCommands, CompilerConfig, MemoryFiles, OpenDocuments, Session,
    SessionFiles, WatchFilter, file::coordinates::FileOffset,
};

/// A session over one file, cooked, with the offset of the *use* the fixture is about.
fn expanded(source: &str, at: &str) -> String {
    expanded_with(source, at, CompilerConfig::default())
}

/// The same, under a configuration the caller chose — a compile database, for the spelling of `__FILE__`.
fn expanded_with(source: &str, at: &str, config: CompilerConfig) -> String {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), config);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let offset = source.find(at).expect("the fixture uses the marker") + 1;
    session
        .the_expansion_of_a_macro(&view, FileOffset::new(offset))
        .unwrap_or_else(|| panic!("`{at}` at {offset} has no expansion"))
}

/// **`##` pastes the two tokens into one**, which is the operator that makes the result a different *identifier*
/// rather than a different arrangement of tokens.
#[test]
fn a_macro_expansion_applies_the_paste_operator() {
    let source = "\
#define PASTE(a, b) a ## b
int PASTE(foo, bar) = 1;
";
    let expansion = expanded(source, "PASTE(foo");
    assert_eq!(expansion.trim(), "foobar", "`a ## b` is one token");
}

/// **The macros the body names are expanded too**, recursively — what a compiler does and what a one-pass
/// substitution cannot show.
#[test]
fn a_macro_expansion_expands_the_macros_in_its_body() {
    let source = "\
#define ORIGIN 3
#define WRAPPED (ORIGIN + 1)
int x = WRAPPED;
";
    let expansion = expanded(source, "WRAPPED;");
    assert!(
        expansion.contains('3'),
        "`ORIGIN` is a macro and its value is what the compiler sees: {expansion}"
    );
    assert!(
        !expansion.contains("ORIGIN"),
        "the name is substituted: {expansion}"
    );
}

/// **`#` stringizes its argument.**
#[test]
fn a_macro_expansion_applies_the_stringize_operator() {
    let source = "\
#define STR(x) #x
const char *s = STR(hello);
";
    let expansion = expanded(source, "STR(hello)");
    assert!(
        expansion.contains("\"hello\""),
        "`#x` is a string literal: {expansion}"
    );
}

/// **The expansion is formatted**, which is what makes it readable as code rather than as the body's own spacing.
#[test]
fn a_macro_expansion_is_formatted() {
    let source = "\
#define ADD(a,b) ((a)+(b))
int n = ADD(1,   2);
";
    let expansion = expanded(source, "ADD(1");
    assert_eq!(
        expansion.trim(),
        "((1) + (2))",
        "the formatter puts the operators where a reader expects them"
    );
}

/// **`__LINE__` is the line of the use, not of the card's synthetic source.** The card builds one `#define` line per
/// definition and then the invocation, so a call written on line 3 used to expand to `2`; the source is padded so the
/// invocation sits on its own line.
#[test]
fn a_macro_expansion_answers_the_line_of_the_use() {
    let source = "\
#define LINE __LINE__
void f() {
    int line = LINE;
}
";
    let expansion = expanded(source, "LINE;");
    assert_eq!(expansion.trim(), "3", "the call is on line 3: {expansion}");
}

/// **`__FILE__` is the file the use is written in.** It used to be the empty literal in every expansion.
#[test]
fn a_macro_expansion_answers_the_file_of_the_use() {
    let source = "\
#define FILE_ __FILE__
const char *file = FILE_;
";
    let expansion = expanded(source, "FILE_;");
    assert_eq!(
        expansion.trim(),
        "\"/p/a.cpp\"",
        "the file this fixture is written in: {expansion}"
    );
}

/// **And with a compile database, the command's spelling wins** — a compiler reports the name it was invoked with,
/// not the path the analysis opens, and the card is asked for the compiler's answer.
#[test]
fn a_macro_expansion_answers_the_commands_spelling_of_the_file() {
    let source = "\
#define FILE_ __FILE__
const char *file = FILE_;
";
    let config = CompilerConfig {
        database: Some(std::sync::Arc::new(CompileCommands {
            commands: vec![CompileCommand {
                file: std::path::PathBuf::from("/p/a.cpp"),
                file_as_written: "src/a.cpp".to_string(),
                directory: Some(std::path::PathBuf::from("/p")),
                arguments: Vec::new(),
            }],
            malformed: 0,
        })),
        ..CompilerConfig::default()
    };
    let expansion = expanded_with(source, "FILE_;", config);
    assert_eq!(
        expansion.trim(),
        "\"src/a.cpp\"",
        "the spelling the compile command used: {expansion}"
    );
}

/// **A `#line` in force before the use is what the card reports.** The card cooks a synthetic source, so it places the
/// invocation with a `#line` of its own; without that the answer was the physical line the synthetic source happened to
/// put it on.
#[test]
fn a_macro_expansion_honours_a_line_directive() {
    let line = "\
#define LINE __LINE__
#line 100 \"x.c\"
void f() {
    int line = LINE;
}
";
    assert_eq!(
        expanded(line, "LINE;").trim(),
        "101",
        "line 4 of the file is 101 after `#line 100` on line 2"
    );

    let file = "\
#define FILE_ __FILE__
#line 100 \"x.c\"
void f() {
    const char *f = FILE_;
}
";
    assert_eq!(
        expanded(file, "FILE_;").trim(),
        "\"x.c\"",
        "and the name the directive states is the one the card reports"
    );
}

/// **A statement's semicolon stays on its block's line.** `TEXTLOCK_WRAP`'s body writes `code;`, and neovim calls it
/// with a braced block, so the expansion is `{ … };` — the formatter used to put the `;` alone on the next line.
#[test]
fn a_block_argument_keeps_its_semicolon_on_the_brace() {
    let source = "\
#define WRAP(code) do { code; } while (0)
void f(void) { WRAP({ g(); }); }
";
    let expansion = expanded(source, "WRAP({");
    assert!(
        !expansion.lines().any(|line| line.trim() == ";"),
        "the semicolon is on a line of its own: {expansion}"
    );
    assert!(
        expansion.contains("};"),
        "the block's brace and `;` are together: {expansion}"
    );
}
