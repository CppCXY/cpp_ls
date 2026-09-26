//! **M2's acceptance**: our cooked token stream against the compiler's own preprocessor.
//!
//! ```text
//! cargo run -q -p cpp_code_analysis --example cook_vs_compiler                 # the built-in fixture
//! cargo run -q -p cpp_code_analysis --example cook_vs_compiler -- <file.cpp>   # any self-contained file
//! ```
//!
//! The two sides are: the compiler's `-E` output (its own preprocessing, the ground truth), and
//! [`cpp_code_analysis::cook`] over the same file. Both are lexed with the same lexer, trivia is dropped on
//! both sides, and the **spellings** are compared in order. What is being checked is not formatting — a
//! preprocessor is allowed to lay tokens out however it likes — but *which tokens there are*: which branch
//! survived, what each macro expanded to, and what `#`/`##` produced.
//!
//! # What makes a file comparable
//!
//! * **No `#include`.** A file's cooked stream stops at its own tokens; a translation unit's does not. Cooking
//!   a TU means cooking each file and stitching them in include order, which is the next step.
//! * **No compiler builtins** (`__FILE__`, `_MSC_VER`, …). We do not have them at level 0, and a fixture that
//!   asked about them would be measuring the configuration rather than the cooking.
//!
//! Both holds for the built-in fixture. A real header with its `#include` lines removed is also comparable —
//! the macros the headers defined are unknown to *both* sides, and both then apply C's rule that an
//! undefined identifier is `0`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A file that exercises the whole job: object-like and function-like macros, arguments used twice,
/// `#`/`##`, nesting, `#if`/`#elif`/`#else`, a `#define` in a branch nobody compiles, and `#undef`.
const FIXTURE: &str = r#"// A self-contained translation unit: nothing is included, nothing is predefined.
#define VERSION 3
#define NAME(x) "lib" #x
#define CAT(a, b) a##b
#define TWICE(x) ((x) + (x))
#define WRAP(x) [ x ]
#define MAX(a, b) ((a) > (b) ? (a) : (b))

#if VERSION >= 3
#define IS_NEW 1
#elif VERSION == 2
#define IS_NEW 0
#else
#define IS_NEW 0
#endif

#if 0
#define NEVER 1
int compiled_when_wrong = NEVER;
#else
int not_compiled = 0;
#endif

#ifdef NEVER
int wrong;
#endif

#ifndef IS_NEW
int also_wrong;
#endif

#undef VERSION
#define VERSION 4

CAT(in, t) main() {
    WRAP(int) a = TWICE(VERSION);
    WRAP(int) b = MAX(a, 7);
    const char* n = NAME(version);
    return b;
}
"#;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mut file: Option<PathBuf> = None;
    let mut compiler: Option<PathBuf> = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--compiler" => compiler = arguments.next().map(PathBuf::from),
            other => file = Some(PathBuf::from(other)),
        }
    }

    let (file, generated) = match file {
        Some(file) => (file, None),
        None => {
            let directory = std::env::temp_dir().join("cppls-cook-vs-compiler");
            std::fs::create_dir_all(&directory).expect("the probe directory");
            let path = directory.join("fixture.cpp");
            std::fs::write(&path, FIXTURE).expect("the fixture");
            (path, Some(directory))
        }
    };

    let Some(compiler) = compiler.or_else(find_a_compiler) else {
        println!(
            "no compiler to compare against on this machine — set CXX, or pass --compiler <path>. \
             The cooked stream is still exercised by `tests/cooked.rs`."
        );
        return;
    };

    let source = std::fs::read_to_string(&file).expect("the file");
    let theirs = match preprocess_with(&compiler, &file) {
        Some(text) => text,
        None => {
            println!("{} could not preprocess the file", compiler.display());
            return;
        }
    };

    // The macros the compiler defines before it reads anything (`-dM`). Feeding them to our side is the
    // difference between level 0 and level 1 of the architecture's ladder: without them `#ifdef __cplusplus`
    // is decided by C's "an undefined identifier is 0" rule on our side and by the real value on theirs, so
    // the two streams are then answering *different* questions.
    let builtins = builtins_of(&compiler);
    let ours = {
        let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
        cpp_code_analysis::cook_with(&source, &tokens, &builtins)
    };

    // Our own side is read from the tokens' **spellings**, not from the source at their ranges: a pasted
    // token's text (`x##y` → `xy`) is a spelling the file does not contain anywhere, and a stringized one
    // (`#x` → `"x"`) is invented by the operator. That is what `ExpandedToken::token.text` is for.
    let our_words: Vec<String> = ours
        .tokens
        .iter()
        .map(|token| token.token.text().to_string())
        .collect();
    let their_words = significant_words(&theirs, lexed_ranges(&theirs).into_iter());
    println!("file        {}", file.display());
    println!("compiler    {}", compiler.display());
    println!(
        "ours        {} tokens | inactive spans {} | assumed-undefined branches {} | diagnostics {}",
        our_words.len(),
        ours.inactive.len(),
        ours.assumed_undefined,
        ours.diagnostics.len()
    );
    println!("theirs      {} tokens", their_words.len());

    match first_difference(&our_words, &their_words) {
        None if our_words.len() == their_words.len() => {
            println!("\nthe token sequences agree.");
        }
        None => println!("\nthe shorter sequence is a prefix of the other — one side stopped early."),
        Some(index) => {
            println!("\nthe first difference is at token {index}:");
            for offset in 0..3 {
                let index = index.saturating_add(offset).saturating_sub(1);
                println!(
                    "  ours   {:<24} theirs {}",
                    our_words.get(index).cloned().unwrap_or_default(),
                    their_words.get(index).cloned().unwrap_or_default()
                );
            }
        }
    }

    if let Some(directory) = generated {
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// The compiler's own predefined macros, as a macro table.
///
/// `-dM -E` prints its macro table instead of a translation unit, and what it prints is a file's worth of
/// `#define` lines — so they are read the way any other `#define` is, by the same
/// [`parse_define`](cpp_code_analysis::preprocess::macros::parse_define) the directive layer uses. That is
/// the point of doing it here rather than with a private format: a builtin is a macro like any other, and
/// a second reader would be a second answer to what one is.
fn builtins_of(compiler: &Path) -> cpp_code_analysis::MacroTable {
    let mut table = cpp_code_analysis::MacroTable::new();

    let Ok(output) = Command::new(compiler)
        .args(["-dM", "-E", "-x", "c++", "-"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return table;
    };

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(definition) = line.strip_prefix("#define ") else {
            continue;
        };

        // `#define NAME(args) body` and `#define NAME body` are both just `NAME…` to `parse_define`, which
        // is what the directive layer hands it: the directive's own name and `#` are already gone.
        let (tokens, _) = cpp_parser::lex(definition, &cpp_parser::LexerConfig::default());
        let range = cpp_parser::SourceRange::new(0, definition.len());
        let tokens: Vec<cpp_code_analysis::Token> = tokens
            .iter()
            .map(|token| {
                cpp_code_analysis::Token::new(
                    token.kind,
                    &definition[token.range.start_offset..token.range.end_offset()],
                    token.range,
                )
            })
            .collect();

        if let Some(definition) =
            cpp_code_analysis::preprocess::macros::parse_define(&tokens, range)
        {
            table.define(definition);
        }
    }

    table
}

/// The compiler's own preprocessing of a file, as text: `-E -P` for a GNU-like compiler.
///
/// `-P` drops the line markers, which is what makes the output a token sequence rather than a token sequence
/// with a table of file positions in it. A compiler that does not take `-P` prints them anyway, and they are
/// skipped by the comparison — a line beginning with `#` is a directive the preprocessor left behind, not a
/// token of the program.
fn preprocess_with(compiler: &Path, file: &Path) -> Option<String> {
    let output = Command::new(compiler)
        .arg("-E")
        .arg("-P")
        .arg(file)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A compiler to compare against: `CXX` first, then the usual names.
fn find_a_compiler() -> Option<PathBuf> {
    if let Some(cxx) = std::env::var_os("CXX") {
        let path = PathBuf::from(cxx);
        if path.exists() {
            return Some(path);
        }
    }

    ["g++", "clang++", "c++"].iter().find_map(|name| {
        let output = Command::new(name).arg("--version").output().ok()?;
        output.status.success().then(|| PathBuf::from(name))
    })
}

/// The ranges of the significant tokens of a text, lexed with our own lexer.
///
/// **Directive lines are dropped first**, and that is the convention the comparison rests on: a preprocessor
/// passes `#pragma` and a `#line` through to its output, while a cooked stream has no directives in it at
/// all — a directive is a line, not a construct, and nothing downstream parses one. Leaving them in would
/// compare "did we keep the same directives" (we by design do not) instead of "did we keep the same tokens".
fn lexed_ranges(text: &str) -> Vec<cpp_parser::SourceRange> {
    let without_directives: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let (tokens, _) = cpp_parser::lex(&without_directives, &cpp_parser::LexerConfig::default());
    tokens
        .iter()
        .filter(|token| !cpp_parser::is_trivia(token.kind))
        .map(|token| token.range)
        .collect()
}

/// The spellings of the given ranges, in order.
fn significant_words(
    text: &str,
    ranges: impl Iterator<Item = cpp_parser::SourceRange>,
) -> Vec<String> {
    let without_directives: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    ranges
        .map(|range| without_directives[range.start_offset..range.end_offset()].to_string())
        .collect()
}

/// Where two sequences first disagree.
fn first_difference(ours: &[String], theirs: &[String]) -> Option<usize> {
    ours.iter()
        .zip(theirs)
        .position(|(left, right)| left != right)
}
