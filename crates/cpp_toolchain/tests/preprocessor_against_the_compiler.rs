//! **The preprocessor against the compiler that will read the same file.**
//!
//! Every rule in this crate is a claim about what a compiler does — which `#if` branch is compiled, what a macro
//! invocation expands to — and the only witness for such a claim is the compiler itself. These two fixtures are
//! therefore **differential**: the same file goes through `g++ -E` and through [`cook_with`], and the answers are
//! compared.
//!
//! ```text
//! conditionals   a marker token per branch; the set that survives `-E` is the set that must survive the cook
//! expansion      a token stream; `-E`'s tokens and the cook's tokens must be the same, in the same order
//! ```
//!
//! The fixtures are **self-contained on purpose** — no `#include`, no compiler builtins — so the comparison is about
//! this crate's rules and not about which standard library is installed. A compiler that cannot be found skips the
//! test rather than failing it: the analysis is built and tested on machines where no toolchain is installed, and a
//! fixture that only passes with one would be a fixture that gets deleted.
//!
//! `CPPLS_TEST_CXX` names the compiler to use; otherwise `g++`, `clang++` and `gcc` are looked for on `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

/// The compiler these fixtures are checked against, if this machine has one.
fn the_compiler() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("CPPLS_TEST_CXX") {
        let named = PathBuf::from(named);
        if named.is_file() {
            return Some(named);
        }
    }
    for candidate in ["g++", "clang++", "gcc"] {
        // A run rather than a PATH search: `Command::new` is what will be used, and a name that resolves but cannot
        // run is not a compiler this test can ask.
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

/// The file the two preprocessors read, written once per test.
struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new(name: &str, source: &str) -> Fixture {
        let directory =
            std::env::temp_dir().join(format!("cppls-preprocessor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a directory for the fixture");
        let path = directory.join("fixture.cpp");
        std::fs::write(&path, source).expect("the fixture is written");
        Fixture { directory, path }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// What the compiler makes of the file: its preprocessed translation unit, without line markers.
fn the_compilers_answer(compiler: &Path, file: &Path) -> String {
    // **`-undef -nostdinc`** so that the answer is about the file: no builtin macro and no system header may make a
    // branch taken that this crate never saw. The fixture writes every macro it uses.
    let output = Command::new(compiler)
        .args(["-E", "-P", "-undef", "-nostdinc", "-x", "c++", "-std=c++23"])
        .arg(file)
        .output()
        .expect("the compiler runs");
    assert!(
        output.status.success(),
        "the compiler refused the fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// What this crate makes of the same text, through the same entry point a cook uses.
fn our_answer(source: &str) -> String {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    cook_with(source, &tokens, &nothing_defined).render().text
}

/// The significant tokens of a text — what the two answers are compared as.
///
/// Whitespace is the one thing a preprocessor is allowed to arrange differently: `-E` puts a line per directive's
/// residue, and a rendering keeps the file's own layout. A token sequence is what both actually agree on.
fn the_tokens_of(text: &str) -> Vec<String> {
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                cpp_parser::CppTokenKind::None
                    | cpp_parser::CppTokenKind::Whitespace
                    | cpp_parser::CppTokenKind::Newline
                    | cpp_parser::CppTokenKind::LineComment
                    | cpp_parser::CppTokenKind::BlockComment
                    | cpp_parser::CppTokenKind::LineContinuation
                    | cpp_parser::CppTokenKind::DocTrivia
                    | cpp_parser::CppTokenKind::Eof
            )
        })
        .map(|token| text[token.range.start_offset..token.range.end_offset()].to_string())
        .collect()
}

/// The marker tokens, in the order they appear.
fn the_markers_of(text: &str) -> Vec<String> {
    the_tokens_of(text)
        .into_iter()
        .filter(|token| token.starts_with("MARK_"))
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// 1. Which branch is compiled
// ---------------------------------------------------------------------------------------------------------------

/// **Every conditional shape writes a marker in the branch a compiler must take.**
///
/// The branches a compiler drops are the ones a language server must drop — and the branches it *keeps* are what
/// makes a fact visible, so an answer that is wrong in either direction is wrong for every consumer above it.
/// `defined()` on an empty `#define`, a function-like macro used as a condition, `#elif` after a taken `#if`, an
/// `#undef` mid-file and a nested `#if` are the shapes that have each been a defect here at some point.
const CONDITIONALS: &str = "\
#define ONE 1
#define ZERO 0
#define EMPTY
#define ALIAS ONE
#define ID(x) (x)

#if ONE
MARK_IF_ONE
#endif

#if ZERO
MARK_IF_ZERO
#endif

#if !ZERO
MARK_IF_NOT_ZERO
#endif

#if defined(ONE) && !defined(MISSING)
MARK_DEFINED
#endif

#if ONE + ONE == 2
MARK_ARITHMETIC
#endif

#if ONE > ZERO
MARK_COMPARISON
#endif

#if ID(ONE)
MARK_FUNCTION_LIKE
#endif

#if ALIAS
MARK_ALIAS
#endif

#ifdef EMPTY
MARK_EMPTY_DEFINED
#endif

#if ONE
MARK_IF_TAKEN
#elif 1
MARK_ELIF_AFTER_ONE
#else
MARK_ELSE_AFTER_ONE
#endif

#if ZERO
MARK_IF_OF_ZERO
#elif ONE
MARK_ELIF_OF_ZERO
#else
MARK_ELSE_OF_ZERO
#endif

#ifndef MISSING
MARK_IFNDEF
#endif

#if 1
#if ZERO
MARK_INNER_INACTIVE
#else
MARK_INNER_ELSE
#endif
#endif

#undef ONE
#if defined(ONE)
MARK_AFTER_UNDEF
#endif

#if !defined(ONE)
MARK_AFTER_UNDEF_NEGATED
#endif
";

#[test]
fn which_branch_is_compiled_is_what_the_compiler_says() {
    let Some(compiler) = the_compiler() else {
        eprintln!("skipped: no C++ compiler on this machine (set CPPLS_TEST_CXX to name one)");
        return;
    };
    let fixture = Fixture::new("conditionals", CONDITIONALS);

    let theirs = the_markers_of(&the_compilers_answer(&compiler, &fixture.path));
    let ours = the_markers_of(&our_answer(CONDITIONALS));

    assert_eq!(
        ours,
        theirs,
        "\nthe branches this crate compiles are not the ones `{}` compiles\n  ours:   {ours:?}\n  theirs: {theirs:?}",
        compiler.display()
    );
}

// ---------------------------------------------------------------------------------------------------------------
// 2. What an invocation expands to
// ---------------------------------------------------------------------------------------------------------------

/// **Every expansion shape, written so that the answer is a token stream and not a value.**
///
/// Object-like, function-like, empty, nested, self-referential (`SELF` → `SELF`, which a compiler leaves alone),
/// stringified, pasted, variadic, redefinition and `#undef`. The comparison is the *whole* token stream, so a state
/// that leaks from one line to the next shows up as well.
const EXPANSIONS: &str = "\
#define ID(x) x
#define ADD(a, b) ((a) + (b))
#define TWICE(x) ADD(x, x)
#define STRINGIFY(x) #x
#define PASTE(a, b) a##b
#define CALL(f, ...) f(__VA_ARGS__)
#define EMPTY_EXPANDS
#define SELF SELF
#define A B
#define B 1
#define REDEFINED 1

ID(one)
ADD(1, 2)
TWICE(3)
STRINGIFY(a b)
PASTE(foo, bar)
CALL(ID, 7)
EMPTY_EXPANDS tail
SELF
A
REDEFINED

#undef REDEFINED
#define REDEFINED 2
REDEFINED
";

#[test]
fn what_an_invocation_expands_to_is_what_the_compiler_says() {
    let Some(compiler) = the_compiler() else {
        eprintln!("skipped: no C++ compiler on this machine (set CPPLS_TEST_CXX to name one)");
        return;
    };
    let fixture = Fixture::new("expansions", EXPANSIONS);

    let theirs = the_tokens_of(&the_compilers_answer(&compiler, &fixture.path));
    let ours = the_tokens_of(&our_answer(EXPANSIONS));

    assert_eq!(
        ours,
        theirs,
        "\nthe tokens this crate produces are not the ones `{}` produces\n  ours:   {ours:?}\n  theirs: {theirs:?}",
        compiler.display()
    );
}
