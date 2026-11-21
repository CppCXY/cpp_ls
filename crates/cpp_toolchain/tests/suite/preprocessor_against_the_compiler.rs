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

/// **A parameter may be spelled with a keyword** — the preprocessor's grammar has no keywords at all.
///
/// `#define createBoolConfig(name, alias, flags, config_addr, default, is_valid, apply) …` is redis 8.x's
/// `src/config.c`, and `default` is a keyword by then. Read as an identifier only, the parameter list was cut there,
/// the rest of it became the macro's **body**, and the macro became object-like — so redis's `standardConfig
/// static_configs[]` stayed unexpanded text and reported **952 syntax errors** (`docs/c-support.md` §8).
///
/// The compiler is the oracle: `gcc -E` expands both the params and the invocation, so the whole token stream is
/// comparable.
#[test]
fn a_parameter_spelled_with_a_keyword_expands_as_the_compiler_says() {
    let Some(compiler) = the_compiler() else {
        eprintln!("skipped: no C++ compiler on this machine (set CPPLS_TEST_CXX to name one)");
        return;
    };
    let source = "\
#define KEYWORD_PARAMETERS(a, default, struct) ((a) + (default) + (struct))
#define NESTED_ONE(name, default) \\
    .name = (name), \\
    .value = (default),
KEYWORD_PARAMETERS(1, 2, 3)
NESTED_ONE(one, 2)
";
    let fixture = Fixture::new("keyword-parameters", source);
    let theirs = the_tokens_of(&the_compilers_answer(&compiler, &fixture.path));
    let ours = the_tokens_of(&our_answer(source));
    assert_eq!(
        ours,
        theirs,
        "\nthe tokens this crate produces are not the ones `{}` produces\n  ours:   {ours:?}\n  theirs: {theirs:?}",
        compiler.display()
    );
}

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

/// **A two-character operator in a macro body is still one operator after the cook.**
///
/// The lexer deliberately splits `>>`, `>=` and `>>=` into one-byte tokens (`CppLexer::push`) so the grammar can put
/// `std::vector<std::vector<int>>` back together by byte adjacency, and the renderer's rule is that tokens that
/// touched stay touching. A body's tokens come from the `#define`'s own text, so they touched — and a rendering that
/// writes `> =` gives the parser two tokens it will not join: measured on Lua 5.5.0's `lobject.h`,
/// `#define strisshr(ts) ((ts)->shrlen >= 0)` rendered as `( ( s ) -> shrlen > = 0 )` in the program reading of
/// `ldebug.c`, which is where 15 `expected primary expression` errors came from.
#[test]
fn an_operator_in_a_macro_body_keeps_its_characters_together() {
    let source = "\
#define strisshr(ts) ((ts)->shrlen >= 0)
#define below(a, b) ((a) <= (b))
#define shift(a) ((a) >> 2)
#define both(a) ((a) >>= 1)
int f(struct S *s, int a) {
    return strisshr(s) + below(a, 1) + shift(a) + both(a);
}
";
    let rendered = our_answer(source);
    println!("{rendered}");
    for (together, apart) in [
        (">=", "> ="),
        ("<=", "< ="),
        (">>", "> >"),
        (">>=", "> > ="),
    ] {
        assert!(
            rendered.contains(together),
            "`{together}` is one operator and the rendering must spell it: {rendered}"
        );
        assert!(
            !rendered.contains(apart),
            "`{together}` was rendered as `{apart}`, which the parser cannot put back together: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 3. Which regions of a file are inactive — the answer a reader sees greyed out
// ---------------------------------------------------------------------------------------------------------------

/// The spans this crate greys out: the regions the walk decided not to compile.
fn our_inactive(source: &str) -> Vec<cpp_parser::SourceRange> {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let nothing_defined = MacroTable::new();
    cook_with(source, &tokens, &nothing_defined).inactive
}

/// Every `MARK_*` identifier and the offset it starts at.
fn marker_offsets(source: &str) -> Vec<(String, usize)> {
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    tokens
        .iter()
        .filter(|token| token.kind == cpp_parser::CppTokenKind::Identifier)
        .map(|token| {
            (
                source[token.range.start_offset..token.range.end_offset()].to_string(),
                token.range.start_offset,
            )
        })
        .filter(|(text, _)| text.starts_with("MARK_"))
        .collect()
}

/// The markers **outside** every inactive span — the code a reader sees compiled.
fn markers_outside(spans: &[cpp_parser::SourceRange], markers: &[(String, usize)]) -> Vec<String> {
    markers
        .iter()
        .filter(|(_, at)| {
            !spans
                .iter()
                .any(|span| span.start_offset <= *at && *at < span.end_offset())
        })
        .map(|(text, _)| text.clone())
        .collect()
}

fn covers(source: &str, span: &cpp_parser::SourceRange, needle: &str) -> bool {
    let at = source
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is in the fixture"));
    span.start_offset <= at && at < span.end_offset()
}

/// **The branches a reader must see greyed out are the ones the compiler does not compile.**
///
/// The feature's whole risk is a wrong region: grey text over code that compiles is visible wrongness, and no
/// downstream layer can catch it. So the comparison is differential, like the two fixtures above: every marker the
/// compiler keeps must be outside the spans, and every marker it drops must be inside one.
#[test]
fn the_regions_a_reader_sees_greyed_are_the_ones_the_compiler_drops() {
    let Some(compiler) = the_compiler() else {
        eprintln!("skipped: no C++ compiler on this machine (set CPPLS_TEST_CXX to name one)");
        return;
    };
    let fixture = Fixture::new("inactive-regions", INACTIVE_REGIONS);

    let theirs = the_markers_of(&the_compilers_answer(&compiler, &fixture.path));
    let spans = our_inactive(INACTIVE_REGIONS);
    let ours = markers_outside(&spans, &marker_offsets(INACTIVE_REGIONS));

    assert_eq!(
        ours,
        theirs,
        "\nthe branches this crate would grey out are not the ones `{}` drops\n  visible: {ours:?}\n  theirs:  {theirs:?}",
        compiler.display()
    );
    assert!(
        !spans.is_empty(),
        "the fixture has dead branches, so there is something to grey out"
    );
}

/// Every conditional shape the compiler compares, each with a marker in the branch it keeps.
const INACTIVE_REGIONS: &str = "\
#define ON 1
#define OFF 0

#if ON
MARK_LIVE_IF
#else
MARK_DEAD_ELSE
#endif

#if OFF
MARK_DEAD_IF
#elif ON
MARK_LIVE_ELIF
#else
MARK_DEAD_ELSE_OF_ELIF
#endif

#ifndef MISSING
MARK_LIVE_IFNDEF
#endif

#if ON
#if OFF
MARK_DEAD_NESTED
#else
MARK_LIVE_NESTED_ELSE
#endif
#endif

#if OFF
#if ON
MARK_DEAD_IN_DEAD
#else
MARK_DEAD_ELSE_IN_DEAD
#endif
#endif
";

/// **One dead branch is one span, and an `#include` inside it is never read.**
///
/// The include names a header that does not exist: a compiler that entered the branch would fail the fixture, so the
/// compiler's own success is the proof of the property the plan turns on (`docs/inactive-regions.md` §2.4). On this
/// side, the span must cover both the `#include` line and the marker after it, which is what makes a dead branch one
/// range a client can grey out.
#[test]
fn one_dead_branch_is_one_span_and_its_include_is_never_read() {
    let Some(compiler) = the_compiler() else {
        eprintln!("skipped: no C++ compiler on this machine (set CPPLS_TEST_CXX to name one)");
        return;
    };
    let fixture = Fixture::new("dead-include", DEAD_INCLUDE);

    let theirs = the_markers_of(&the_compilers_answer(&compiler, &fixture.path));
    assert_eq!(
        theirs,
        ["MARK_LIVE_BEFORE", "MARK_LIVE_AFTER"],
        "the compiler must not read the header the dead branch includes"
    );

    let spans = our_inactive(DEAD_INCLUDE);
    assert_eq!(
        spans.len(),
        1,
        "one dead branch is one span, not a piece per line: {spans:?}"
    );
    let span = &spans[0];
    assert!(
        covers(DEAD_INCLUDE, span, "#include"),
        "the span covers the include line: {span:?}"
    );
    assert!(
        covers(DEAD_INCLUDE, span, "MARK_DEAD_AFTER_THE_INCLUDE"),
        "and the code after it: {span:?}"
    );
    for live in ["MARK_LIVE_BEFORE", "MARK_LIVE_AFTER"] {
        let at = DEAD_INCLUDE.find(live).unwrap();
        assert!(
            !(span.start_offset <= at && at < span.end_offset()),
            "`{live}` is compiled and must not be greyed"
        );
    }
}

const DEAD_INCLUDE: &str = "\
#define ON 1

#if ON
MARK_LIVE_BEFORE
#else
#include \"a_header_that_does_not_exist.h\"
MARK_DEAD_AFTER_THE_INCLUDE
#endif

MARK_LIVE_AFTER
";
