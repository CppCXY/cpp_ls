//! **Which literals are format strings, and in whose syntax** — the locator, asked of a file.
//!
//! The scanner (`tests/format_scanner.rs`) answers what is inside one literal; this is the other half: which literal
//! of a file is a format string at all. Every case here is a *whole file*, because that is the only way to ask the
//! questions that matter — which function was called, what its name resolved to, and which argument the literal is.
//!
//! ```text
//! cargo test -p cpp_code_analysis --test format
//! ```
//!
//! # The two shapes the parser writes, and why this test has both
//!
//! `std::format("…", x)` in an initializer is a `CallExpr`; `std::println("…", x);` as a statement is a **declaration**
//! — the most-vexing-parse reading of a statement that begins with a qualified name and a `(`. Both are ordinary C++
//! and both carry a format string, so the locator walks the **token stream** rather than the tree and this file holds
//! a case of each. If the parser ever learns to read the statement as a call, these tests keep passing, which is the
//! point.

use cpp_code_analysis::format::{ExtraFunction, FormatConfig, FormatKind, PieceKind};
use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn a_session(source: &str) -> (Session<MemoryFiles>, std::path::PathBuf) {
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    (session, std::path::PathBuf::from("/p/a.cpp"))
}

/// **Every format string of a file**, rendered as `kind literal [pieces]` — one line per literal, in source order.
///
/// The rendering is the whole assertion: a reader of the test can see which run of bytes is which kind, and a byte
/// range is not a thing a person can check by eye. A piece is written `kind:text`, so `specifier:{:.3
/// conversion:f specifier:}` is a `{:.3f}` field.
fn formats(source: &str, config: &FormatConfig) -> Vec<String> {
    let (session, path) = a_session(source);
    let view = session.view_of_the_file(&path).expect("a view of the file");
    session
        .format_literals(&view, config)
        .into_iter()
        .map(|literal| {
            let text = &source[literal.range.start_offset..literal.range.end_offset()];
            let pieces: Vec<String> = literal
                .pieces
                .iter()
                .map(|piece| {
                    format!(
                        "{}:{}",
                        match piece.kind {
                            PieceKind::Specifier => "specifier",
                            PieceKind::Argument => "argument",
                            PieceKind::Conversion => "conversion",
                            PieceKind::Escape => "escape",
                            PieceKind::DynamicWidth => "dynamic",
                        },
                        &text[piece.start..piece.end]
                    )
                })
                .collect();
            format!(
                "{} {text} [{}]",
                match literal.kind {
                    FormatKind::Std => "std",
                    FormatKind::Printf => "printf",
                    FormatKind::Scanf => "scanf",
                },
                pieces.join(" ")
            )
        })
        .collect()
}

fn everything() -> FormatConfig {
    FormatConfig::default()
}

/// **`std::format` in an expression** — the shape the parser reads as a call, and the one the doc's acceptance is
/// written about (`main.cpp` in the project this was built against is exactly this line).
#[test]
fn a_qualified_format_call_is_read_out_of_an_initializer() {
    let source = r#"
void f() {
    std::string message = std::format("Hello, {}!", "world");
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![r#"std "Hello, {}!" [specifier:{}]"#]
    );
}

/// **`std::println` as a statement**, which the parser reads as a *declaration* and which is nevertheless a call with
/// a format string in it. This is the case that made the locator a token walk.
#[test]
fn a_statement_that_begins_with_a_qualified_call_is_still_read() {
    let source = r#"
void f() {
    std::println("值 = {:.3f}，宽度 {:{}}", 3.14159, 9);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            "std \"值 = {:.3f}，宽度 {:{}}\" [specifier:{:.3 conversion:f specifier:} specifier:{: dynamic:{} specifier:}]"
        ]
    );
}

/// **The C library's two families**, and the argument each of them puts its format string in.
#[test]
fn a_c_call_is_read_and_its_argument_position_is_the_library_s_own() {
    let source = r#"
void f(int y, int w, char* s, double d, FILE* out) {
    printf("%-8.2f|%*d|%1$s|%%", d, w, s);
    fprintf(out, "%d", y);
    snprintf(s, 64, "%.2f", d);
    scanf("%3d %[a-z] %n", &y, s, &w);
    sscanf(s, "%d", &y);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            r#"printf "%-8.2f|%*d|%1$s|%%" [specifier:%-8.2 conversion:f specifier:% argument:* conversion:d specifier:% argument:1$ conversion:s escape:%%]"#,
            r#"printf "%d" [specifier:% conversion:d]"#,
            r#"printf "%.2f" [specifier:%.2 conversion:f]"#,
            r#"scanf "%3d %[a-z] %n" [specifier:%3 conversion:d specifier:% conversion:[a-z] specifier:% conversion:n]"#,
            r#"scanf "%d" [specifier:% conversion:d]"#,
        ]
    );
}

/// **A `printf`'s second string is data, not a format string** — the argument position is the whole point of the
/// table, and a scanner that read every literal of a call would paint `"text"` here.
#[test]
fn an_argument_that_happens_to_be_a_string_is_not_a_format_string() {
    let source = r#"
void f() {
    printf("%s", "text");
    fprintf(stderr, "%s", "text");
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            r#"printf "%s" [specifier:% conversion:s]"#,
            r#"printf "%s" [specifier:% conversion:s]"#,
        ]
    );
}

/// **`std::print` has two overloads**, and the literal is the format string in both — but not in `print("{}", "x")`,
/// where the second string is an argument.
#[test]
fn a_print_reads_its_format_string_with_or_without_a_stream() {
    let source = r#"
void f(FILE* stream) {
    std::print("a {} b", 1);
    std::print(stream, "c {} d", 1);
    std::print("e {}", "text");
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            r#"std "a {} b" [specifier:{}]"#,
            r#"std "c {} d" [specifier:{}]"#,
            r#"std "e {}" [specifier:{}]"#,
        ]
    );
}

/// **A literal that is not the whole argument is not read**: `printf(a + "%d")` passes a value, and a format string
/// written across two literals (`"%" PRIu64`) has a macro in the middle, which this pass cannot see into.
#[test]
fn only_a_literal_that_is_a_whole_argument_is_read() {
    let source = r#"
void f(char* a, int x) {
    printf(a + "%d", x);
    printf("%" PRIu64 "\n", x);
    const char* bare = "%d";
}
"#;
    assert_eq!(
        formats(source, &everything()),
        Vec::<String>::new(),
        "none of the three is a format string of a call"
    );
}

/// **A call to something else is not read**, however it is spelled: a project's own `format`, a name that looks like
/// one, and a function whose first argument is a string for its own reasons.
#[test]
fn a_name_that_is_not_one_of_the_library_s_is_left_alone() {
    let source = r#"
namespace app {
    std::string format(const char* text);
    void logf(const char* text);
}
void f() {
    app::format("{}");
    app::logf("%d");
    record("{}", 1);
    std::setw("%d");
}
"#;
    assert_eq!(formats(source, &everything()), Vec::<String>::new());
}

/// **`using namespace std;` makes a bare `format(…)` the library's**, and this is the one question the locator cannot
/// answer from the text: the same spelling is a project's own function when nothing says otherwise.
///
/// The fixture declares `std::format` in this file so that the scope walk can resolve it without a header — what is
/// under test is which namespace the *name* resolves into, not where `<format>` is on this machine.
#[test]
fn an_unqualified_name_is_resolved_before_it_is_read() {
    let source = r#"
namespace std {
    template <class... Args>
    std::string format(const char* text, Args... args);
}

using namespace std;

void f() {
    format("{}", 1);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![r#"std "{}" [specifier:{}]"#],
        "the analysis resolved `format` into `std`"
    );
}

/// **A bare `format` that does not resolve into `std` is not read**, which is the same fixture with the namespace
/// changed — the case a project's own formatter is.
#[test]
fn a_bare_format_outside_std_is_not_read() {
    let source = r#"
namespace app {
    std::string format(const char* text);
}

using namespace app;

void f() {
    format("{}", 1);
}
"#;
    assert_eq!(formats(source, &everything()), Vec::<String>::new());
}

/// **A member call reads its method's name**, and a local one is not the library's.
#[test]
fn a_member_call_is_read_by_its_method_name() {
    let source = r#"
struct Logger {
    void printf(const char* text, ...);
    void format(const char* text);
};
void f(Logger& log) {
    log.printf("%d", 1);
    log.format("{}");
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![r#"printf "%d" [specifier:% conversion:d]"#],
        "the method is the library's name; `format` on an object is not `std::format`"
    );
}

/// **The `fmt` library**, whose grammar is `std::format`'s — its spellings are read, including the versioned
/// namespace it inlines (`fmt::v11::`), and its `printf` is `printf`'s.
#[test]
fn the_fmt_library_is_read_by_its_own_names() {
    let source = r#"
void f() {
    fmt::print("{}\n", 1);
    fmt::format("{:>8}", 1);
    fmt::v11::format("{:d}", 1);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            r#"std "{}\n" [specifier:{}]"#,
            r#"std "{:>8}" [specifier:{:>8}]"#,
            r#"std "{:d}" [specifier:{: conversion:d specifier:}]"#,
        ]
    );
}

/// **`[highlight]`'s `extra` is how a project says its wrapper is a format function** — and it is matched on the
/// resolved name, so a local `logf` that is something else is still left alone.
#[test]
fn a_project_s_own_wrapper_is_read_only_when_it_is_listed() {
    let source = r#"
namespace mylib {
    void logf(const char* text, ...);
}
void f() {
    mylib::logf("a=%d", 1);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        Vec::<String>::new(),
        "nothing says `logf` is a format function"
    );

    let mut config = everything();
    // **The shape `[format.functions]` has after it is read**: a name, the grammar it takes, and which argument the
    // format string is. There is no `extra`/`extra_format` pair any more — one table says both.
    config.functions = vec![ExtraFunction {
        name: "mylib::logf".to_string(),
        syntax: FormatKind::Printf,
        argument: 0,
    }];
    assert_eq!(
        formats(source, &config),
        vec![r#"printf "a=%d" [specifier:% conversion:d]"#],
        "the project listed it"
    );
}

/// **Each family can be turned off on its own**, and a configuration with all of them off costs nothing at all —
/// the whole pass answers before it looks at a token.
#[test]
fn a_family_can_be_turned_off() {
    let source = r#"
void f(int x) {
    std::format("{}", x);
    printf("%d", x);
    scanf("%d", &x);
}
"#;
    let mut config = everything();
    config.printf = false;
    assert_eq!(
        formats(source, &config),
        vec![
            r#"std "{}" [specifier:{}]"#,
            r#"scanf "%d" [specifier:% conversion:d]"#,
        ]
    );

    let off = FormatConfig {
        std_format: false,
        printf: false,
        scanf: false,
        functions: Vec::new(),
    };
    assert_eq!(formats(source, &off), Vec::<String>::new());
}

/// **A file with no format strings costs a walk and nothing else** — the answer is empty rather than wrong, which is
/// what a large translation unit spends its time on.
#[test]
fn a_file_with_no_format_string_answers_nothing() {
    let source = "int f(int a, int b) { return a + b; }\n";
    assert_eq!(formats(source, &everything()), Vec::<String>::new());
}

/// **Every range is inside the file and inside the literal**, and the pieces are inside their literal — the invariant
/// the client's encoding cannot recover from if it is broken.
#[test]
fn every_range_is_inside_its_literal_and_its_file() {
    let source = r#"
void f(int y, double d) {
    printf("%-8.2f|%1$s|%%", d, "");
    std::println("值 = {:.3f} {:{}}", d, 1);
}
"#;
    let (session, path) = a_session(source);
    let view = session.view_of_the_file(&path).expect("a view");
    let found = session.format_literals(&view, &everything());
    assert_eq!(found.len(), 2);

    for literal in &found {
        assert!(literal.range.end_offset() <= source.len());
        let text = &source[literal.range.start_offset..literal.range.end_offset()];
        assert!(text.starts_with('"') || text.starts_with('R'), "{text}");
        let mut previous = 0usize;
        for piece in &literal.pieces {
            assert!(piece.start >= previous, "out of order in {text}");
            assert!(piece.end <= text.len(), "past the literal in {text}");
            assert!(piece.start < piece.end, "empty piece in {text}");
            previous = piece.end;
        }
    }
}

/// **What the walk costs on a real file** — a measurement, not an assertion.
///
/// ```text
/// $env:CPPLS_FILE = "E:\neovim\src\nvim\message.c"
/// cargo test --release -p cpp_code_analysis --test format -- --ignored --nocapture
/// ```
///
/// It prints the token count, how many string literals the file has, how many the walk kept, and the time — next to
/// `classified_names` on the same view, because that is the pass that dominates a `semanticTokens` answer and the
/// question here is whether the format walk is visible in it at all.
#[test]
#[ignore = "a measurement: needs CPPLS_FILE"]
fn what_the_walk_costs() {
    let Some(path) = std::env::var_os("CPPLS_FILE").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set CPPLS_FILE");
        return;
    };
    let source = std::fs::read_to_string(&path).expect("the file is readable");
    let (session, path) = a_session(&source);

    let started = std::time::Instant::now();
    let view = session.view_of_the_file(&path).expect("a view");
    let view_time = started.elapsed();
    let tokens = view.tree.get_tokens().len();
    let literals = view
        .tree
        .get_tokens()
        .iter()
        .filter(|token| token.kind == cpp_parser::CppTokenKind::StringLiteral)
        .count();

    let started = std::time::Instant::now();
    let rounds = 20;
    let mut kept = 0usize;
    for _ in 0..rounds {
        kept = session
            .format_literals(&view, &FormatConfig::default())
            .len();
    }
    let each = started.elapsed() / rounds;

    let started = std::time::Instant::now();
    let names = session.classified_names(&view);
    let classify_time = started.elapsed();

    println!(
        "PROBE {}: {} bytes, {tokens} token(s), {literals} string literal(s), {kept} format string(s) kept | \
         {each:?} per format walk, {} name(s) in {classify_time:?}, view {view_time:?}",
        path.display(),
        source.len(),
        names.len(),
    );
}

/// **`fmt`'s two syntaxes**, and the one name whose argument position depends on which library declares it.
///
/// `fmt` is where `std::format` came from, so `fmt::format` / `fmt::print` take the same `{…}` grammar — but the
/// library also kept the C syntax, and those names are **not** `printf` renamed:
///
/// ```text
/// sprintf(buffer, format, …)     the C library\s — the format string is the SECOND argument
/// fmt::sprintf(format, …)        fmt\s          — the format string is the FIRST
/// ```
///
/// One table keyed by spelling cannot hold both, which is why the scope decides for that one name (see
/// `format::locator::the_format_kind_of`). The other `fmt::` names either agree with their C counterpart
/// (`fmt::printf` is `%` at argument 0, `fmt::fprintf` takes the stream first) or are `fmt`\s alone.
#[test]
fn the_fmt_library_s_two_syntaxes_are_both_read() {
    let source = r#"
void f(int n, char* buf) {
    fmt::printf("%d\n", n);
    fmt::fprintf(stderr, "%d\n", n);
    fmt::sprintf("%d", n);
    std::sprintf(buf, "%d", n);
    fmt::format("{}", n);
    fmt::print("{:>8}", n);
    fmt::format_to(out, "{}", n);
}
"#;
    assert_eq!(
        formats(source, &everything()),
        vec![
            // The printf syntax, at the position each library puts it.
            r#"printf "%d\n" [specifier:% conversion:d]"#,
            r#"printf "%d\n" [specifier:% conversion:d]"#,
            r#"printf "%d" [specifier:% conversion:d]"#,
            r#"printf "%d" [specifier:% conversion:d]"#,
            // …and the std syntax, which is the same grammar `fmt` gave the standard.
            r#"std "{}" [specifier:{}]"#,
            r#"std "{:>8}" [specifier:{:>8}]"#,
            r#"std "{}" [specifier:{}]"#,
        ]
    );
}
