//! **The option matrix**: every style option, exercised against clang-format, recorded as a text target.
//!
//! ```text
//! cargo run --release -p cpp_formatter --example option_matrix -- <clang-format> [--write]
//! ```
//!
//! For each of the [`StyleOption`] keys this formatter knows, and for each sink below:
//!
//! ```text
//! 1. try candidate values until clang-format accepts one that *changes its output* on that sink
//! 2. format the same sink with the same configuration through this crate
//! 3. if the two texts are byte-identical, remember the case (sink, value, clang-format's text)
//! ```
//!
//! The first case that matches is recorded as a test in `crates/cpp_formatter/tests/options.rs`; every option is written
//! to `docs/option-matrix.md` with its status and, when it differs, the first line that differs.
//!
//! # Why several sinks
//!
//! An option is only observable where its construct is. `AllowShortIfStatementsOnASingleLine` needs a short `if`,
//! `PointerAlignment` needs a declarator, `SortIncludes` needs includes; one file cannot show them all without a test
//! that is mostly noise. Each sink is small and focused, and an option is tried against all of them.
//!
//! # What the numbers mean
//!
//! `no effect` means no candidate value changed clang-format's output on any sink — for the Objective-C, Java,
//! JavaScript, Proto, Verilog, C# and TableGen options that is expected (this formatter is a C++ formatter), and for the
//! C++ ones it means the sink does not contain the construct. `observable but different` is the interesting row: the
//! first difference in the report says whether the option needs re-flowing (line breaks) or a rule this crate has not
//! implemented (spaces, indentation, alignment).

use std::path::PathBuf;
use std::process::Command;

use cpp_formatter::Style;
use cpp_formatter::options::StyleOption;

/// The main sink: one of each construct the options can change.
const SINK_MAIN: &str = r#"#include <vector>
#include "local.h"
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define LONG(a) \
    do { \
        (void)(a); \
    } while (0)
namespace ns {
enum class Kind { A, B };
class Widget : public Base {
public:
    Widget() : a_(0), b_(1) {}
    explicit Widget(int a) : a_(a) {}
    virtual ~Widget() = default;
    void run() override;
    int get() const { return a_; }
protected:
    int a_;
private:
    int b_;
};
template <typename T>
concept Sized = requires(T t) { t.size(); };
template <typename T, int N = 3>
struct Buffer {
    T items[N];
};
void Widget::run() {
    int x = 1;
    int *p = &x;
    const int &r = x;
    char c = 'A';
    const char *s = "hello";
    if (x > 0) {
        x = 2;
    } else {
        x = 3;
    }
    switch (x) {
    case 1:
        break;
    default:
        break;
    }
    auto f = [&](int a) -> int { return a + x; };
    std::vector<int> v = {1, 2, 3};
    int y = 0x1F + 1000 + 1500; // after a statement
    /* block */ y += 1;
    (void)p;
    (void)r;
    (void)c;
    (void)s;
    (void)f;
    (void)v;
    (void)y;
}
} // namespace ns
"#;

/// Short bodies: what the `AllowShort*` family and `BraceWrapping` can change.
const SINK_SHORT: &str = r#"int f() { return 1; }
void g() { h(); }
void i(int x) {
    if (x) {
        h();
    }
    for (int k = 0; k < 2; ++k) {
        h();
    }
    while (x > 0) {
        --x;
    }
    do {
        --x;
    } while (x > 0);
    auto l = [](int a) { return a; };
    (void)l;
}
struct S {
    S() {}
    ~S() {}
};
"#;

/// Consecutive declarations: what the `Align*` family can change.
const SINK_ALIGN: &str = r#"void f(int a, int bb, int ccc) {
    int x = 1;
    int longer = 22;
    int longest = 333;
    x = 4;
    longer = 5;
    // one
    int q = 6; // trailing
    int r = 7; // trailing
    struct T {
        int a;
        long bb;
    };
    (void)x;
    (void)longer;
    (void)q;
    (void)r;
}
"#;

/// Declarators, casts, braces and operators: what the `Space*` family and `PointerAlignment` can change.
const SINK_SPACING: &str = r#"void f() {
    int *p = 0;
    int &r = *p;
    int x = (int)1.5;
    char c = 'a';
    bool b = !x;
    int m = a?b:c;
    int t = 1;
    int u = -t;
    auto g = [&](int v) -> int { return v; };
    struct S {
    };
    if (b)
    {
        g(t);
    }
    (void)r;
    (void)c;
    (void)m;
    (void)u;
}
"#;

/// Directives: what `IndentPPDirectives`, `SortIncludes`, `AlignEscapedNewlines` and the macro blocks can change.
const SINK_PREPROCESSOR: &str = r#"#include <vector>
#include <string>
#include "local.h"
#define SHORT(a) \
    (a)
#define ALIGNED(a, b)          \
    do {                       \
        (void)(a);             \
        (void)(b);             \
    } while (0)
int f() {
#if DEBUG
    return 1;
#endif
    return 0;
}
"#;

/// Comments: what `ReflowComments`, `AlignTrailingComments` and the comment spacing options can change.
const SINK_COMMENTS: &str = r#"// A line comment that is quite long and goes on for a while so that a comment reflow has something to wrap at.
/* A block comment that is long enough for the reflow to want to change it, which it does. */
int a = 1; // one
int bb = 2; // two
int c = 3;
// own line
int d = 4; /* block */
void f() {
    // inner
    int e = 5; // inner trailing
}
"#;

/// The sinks, by name, so a test can say which file it is about.
fn sinks() -> [(&'static str, &'static str); 6] {
    [
        ("main", SINK_MAIN),
        ("short", SINK_SHORT),
        ("align", SINK_ALIGN),
        ("spacing", SINK_SPACING),
        ("preprocessor", SINK_PREPROCESSOR),
        ("comments", SINK_COMMENTS),
    ]
}

/// The values to try, in the order most likely to change a given kind of option.
const CANDIDATES: &[&str] = &[
    "true",
    "false",
    "2",
    "1",
    "8",
    "4",
    "0",
    "Left",
    "Right",
    "Middle",
    "Align",
    "DontAlign",
    "BlockIndent",
    "Always",
    "Never",
    "Inline",
    "EmptyLine",
    "None",
    "All",
    "Outer",
    "Inner",
    "Both",
    "Pointer",
    "OnePerLine",
    "Regroup",
    "Preserve",
    "AfterHash",
    "BeforeHash",
    "DerivePointerAlignment",
    "AcrossEmptyLines",
    "AcrossComments",
    "Consecutive",
    "Leave",
    "Ignore",
    "Auto",
    "Short",
    "Block",
    "LF",
    "CRLF",
    "Upper",
    "Lower",
];

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: option_matrix <clang-format> [--write]");
        std::process::exit(2);
    };
    let write = arguments.any(|argument| argument == "--write");

    let directory = std::env::temp_dir().join("cppfmt-option-matrix");
    std::fs::create_dir_all(&directory).expect("a writable temp directory");
    let mut paths: Vec<(String, PathBuf)> = Vec::new();
    for (name, text) in sinks() {
        let path = directory.join(format!("{name}.cpp"));
        std::fs::write(&path, text).expect("write the sink");
        paths.push((name.to_string(), path));
    }

    // **The snippet corpus is part of the sink pool**: each of those files isolates one language feature, which is
    // exactly what an option needs to be observable.
    let snippets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snippets");
    let mut snippet_files: Vec<PathBuf> = std::fs::read_dir(&snippets)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|extension| extension == "cpp"))
                .collect()
        })
        .unwrap_or_default();
    snippet_files.sort();
    for path in snippet_files {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        paths.push((name, path));
    }

    // **Each sink is first normalised by clang-format itself**, so that under the base style it is a fixed point:
    // after this step the only thing that can change a sink is the option under test. Without it every option took the
    // blame for whatever else the hand-written sink made clang-format do (measured: 3 matches against 38 differences).
    let base = base_style();
    let mut baselines: Vec<String> = Vec::new();
    for (_, path) in &paths {
        let text = std::fs::read_to_string(path).expect("read the sink");
        let normalised =
            clang_format_output(&clang_format, path, &base, &text).expect("clang-format reads the sink");
        std::fs::write(path, &normalised).expect("rewrite the sink");
        baselines.push(normalised);
    }

    let mut cases: Vec<(String, String, String, String)> = Vec::new(); // (sink, key, value, expected)
    let mut report: Vec<(String, &'static str, String)> = Vec::new();
    let (mut no_effect, mut rejected) = (0usize, 0usize);

    for option in StyleOption::ALL {
        let key = option.key();
        if key == "BasedOnStyle" {
            continue;
        }
        let mut matched_for_option: Option<(String, String, String)> = None;
        let mut observed_but_different: Option<String> = None;
        // **One sink per option, chosen by the option`s family**: trying every sink for every option is 6x the
        // process spawns for no extra coverage, because an option is only observable where its construct is.
        // **The sinks to try for this option**: its family`s file first (the construct is there), then everything
        // else — the snippet corpus is part of that, because each of those files isolates one language feature.
        let wanted = sink_for(key);
        let mut order: Vec<usize> = Vec::new();
        for (position, (name, _)) in paths.iter().enumerate() {
            if name.as_str() == wanted {
                order.push(position);
            }
        }
        for (position, (name, _)) in paths.iter().enumerate() {
            if name.as_str() != wanted {
                order.push(position);
            }
        }
        // The vocabulary below is a guess for every option, and most guesses are invalid: this budget keeps the sweep a
        // few minutes long instead of the better part of an hour.
        let mut budget = 60usize;
        for position in order {
            if budget == 0 || matched_for_option.is_some() {
                break;
            }
            let (sink_name, path) = &paths[position];
            let source = std::fs::read_to_string(path).expect("read the sink");
            for candidate in CANDIDATES {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let config = format!("{base}\n{key}: {candidate}");
                let Some(expected) = clang_format_output(&clang_format, path, &config, &source) else {
                    rejected += 1;
                    continue;
                };
                if expected == baselines[position] {
                    continue;
                }
                let ours = cpp_formatter::format_file(path, &source, &style_for(&config));
                if ours == expected {
                    matched_for_option = Some((sink_name.clone(), candidate.to_string(), expected));
                    break;
                }
                if observed_but_different.is_none() {
                    observed_but_different = Some(format!("[{sink_name}, {candidate}] {}", first_difference(&expected, &ours)));
                }
            }
        }
        match (matched_for_option, observed_but_different) {
            (Some((sink, value, expected)), _) => {
                report.push((key.to_string(), "yes", format!("[{sink}, {value}]")));
                cases.push((sink, key.to_string(), value, expected));
            }
            (None, Some(detail)) => report.push((key.to_string(), "no", detail)),
            (None, None) => {
                no_effect += 1;
                report.push((key.to_string(), "-", "no effect on the sinks".to_string()));
            }
        }
    }

    println!();
    println!(
        "options: {} · recorded as tests: {} · observable but different: {} · no effect: {} · rejected values: {}",
        report.len(),
        cases.len(),
        report.iter().filter(|(_, status, _)| *status == "no").count(),
        no_effect,
        rejected
    );

    if write {
        write_tests(&cases, &paths, &baselines);
        write_report(&report, cases.len(), no_effect, rejected);
        println!("wrote crates/cpp_formatter/tests/options.rs and docs/option-matrix.md");
    }
}

/// The style both formatters start from, so an option is the only difference.
fn base_style() -> String {
    "BasedOnStyle: LLVM\nIndentWidth: 4\nContinuationIndentWidth: 8\nColumnLimit: 0".to_string()
}

/// **Which sink an option belongs to**, by its family: the option is only observable where its construct is.
fn sink_for(key: &str) -> &'static str {
    if key.starts_with("Align") {
        return "align";
    }
    if key.starts_with("AllowShort")
        || key.starts_with("BraceWrapping")
        || key.starts_with("BreakBefore")
        || key.starts_with("BreakAfter")
        || key.starts_with("BreakConstructor")
        || key.starts_with("BreakInheritance")
        || key.starts_with("BreakTemplate")
        || key == "BracedInitializerIndentWidth"
    {
        return "short";
    }
    if key.starts_with("Space") || key.starts_with("PointerAlignment") || key.starts_with("DerivePointerAlignment") {
        return "spacing";
    }
    if key.starts_with("SortIncludes")
        || key.starts_with("Include")
        || key.starts_with("PPIndent")
        || key == "IndentPPDirectives"
        || key.starts_with("MacroBlock")
        || key == "AlignEscapedNewlines"
        || key.starts_with("IndentAccessModifiers")
    {
        return "preprocessor";
    }
    if key.contains("Comment") {
        return "comments";
    }
    "main"
}

/// This crate's `Style` for a configuration text, starting from the same base as clang-format.
fn style_for(configuration: &str) -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    for line in configuration.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let (key, value) = (key.trim(), value.trim());
            if key != "BasedOnStyle" {
                style.set_from_text(key, value);
            }
        }
    }
    style
}

/// clang-format's answer for `source` under `config`, or `None` when it rejects the configuration.
fn clang_format_output(clang_format: &str, path: &std::path::Path, config: &str, source: &str) -> Option<String> {
    // `-assume-filename` keeps the include sorter's main-header rule the same for every sink.
    let style_argument = format!("-style={{{}}}", config.replace('\n', ", "));
    let _ = std::fs::write(path, source);
    let output = Command::new(clang_format)
        .arg(style_argument)
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"))
}

/// The first line that differs, with both texts — the report needs the *shape* of the difference, not a count.
fn first_difference(expected: &str, ours: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let our_lines: Vec<&str> = ours.lines().collect();
    for index in 0..expected_lines.len().min(our_lines.len()) {
        if expected_lines[index] != our_lines[index] {
            return format!(
                "line {}: expected {:?}, got {:?}",
                index + 1,
                expected_lines[index],
                our_lines[index]
            );
        }
    }
    format!("line counts differ: {} against {}", expected_lines.len(), our_lines.len())
}

/// The sinks, as one constant per name, so the generated tests can refer to them by name.
fn sink_constant(name: &str) -> &'static str {
    match name {
        "main" => "SINK_MAIN",
        "short" => "SINK_SHORT",
        "align" => "SINK_ALIGN",
        "spacing" => "SINK_SPACING",
        "preprocessor" => "SINK_PREPROCESSOR",
        _ => "SINK_COMMENTS",
    }
}

fn write_tests(cases: &[(String, String, String, String)], paths: &[(String, PathBuf)], baselines: &[String]) {
    let mut text = String::new();
    text.push_str("//! **Generated by `examples/option_matrix.rs`** — do not edit by hand.\n");
    text.push_str("//!\n");
    text.push_str("//! One test per style option that this formatter honours *and* whose effect clang-format 22.1.3 shows on the\n");
    text.push_str("//! sink named in the test. The expected text is clang-format's, so each test is a promise about the option\n");
    text.push_str("//! rather than a snapshot of whatever this implementation happened to print.\n");
    text.push_str("//!\n");
    text.push_str("//! Regenerate with:\n");
    text.push_str("//!\n");
    text.push_str("//! ```text\n");
    text.push_str("//! cargo run --release -p cpp_formatter --example option_matrix -- <clang-format> --write\n");
    text.push_str("//! ```\n\n");
    text.push_str("use cpp_formatter::config::Style;\n\n");
    // **The sinks are written out as they were normalised**: the expected text was produced on the normalised
    // file, so the constant has to be that text or the test compares two different inputs.
    let mut used: Vec<&str> = Vec::new();
    for (sink, _, _, _) in cases {
        if !used.contains(&sink.as_str()) {
            used.push(sink);
        }
    }
    for sink in &used {
        let position = paths.iter().position(|(name, _)| name == sink).expect("a known sink");
        text.push_str(&format!(
            "/// The sink the cases of this name were recorded on.\nconst {}: &str = r##\"{}\"##;\n\n",
            sink_constant(sink),
            baselines[position]
        ));
    }
    text.push_str(
        r#"
/// Format `sink` under `configuration` and compare with the oracle's answer, then re-format that answer.
#[track_caller]
fn check_against_the_oracle(sink: &str, configuration: &str, expected: &str) {
    let style = style(configuration);
    let path = std::path::Path::new("sink.cpp");
    let actual = cpp_formatter::format_file(path, sink, &style);
    assert_eq!(actual, expected, "the option is not honoured the way clang-format honours it");
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the oracle's text is not a fixed point");
}

/// The base style plus one option: the string spells out what the test exercises.
fn style(configuration: &str) -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    // The base the case was recorded with, so the option under test is the only difference.
    style.set_from_text("IndentWidth", "4");
    style.set_from_text("ContinuationIndentWidth", "8");
    style.set_from_text("ColumnLimit", "0");
    for line in configuration.lines() {
        if let Some((key, value)) = line.split_once(':') {
            style.set_from_text(key.trim(), value.trim());
        }
    }
    style
}
"#,
    );
    for (sink, key, value, expected) in cases {
        let function = key
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        text.push_str(&format!(
            "\n/// `{key}: {value}` on the `{sink}` sink\n#[test]\nfn option_{function}() {{\n    check_against_the_oracle(\n        {constant},\n        r#\"{key}: {value}\"#,\n        r#\"{expected}\"#,\n    );\n}}\n",
            constant = sink_constant(sink)
        ));
    }
    let path = workspace_root().join("crates/cpp_formatter/tests/options.rs");
    std::fs::write(path, text).expect("write the generated tests");
}

fn write_report(report: &[(String, &'static str, String)], matched: usize, no_effect: usize, rejected: usize) {
    let mut text = String::new();
    text.push_str("# The option matrix\n\n");
    text.push_str("Generated by `cargo run --release -p cpp_formatter --example option_matrix -- <clang-format> --write`.\n\n");
    text.push_str("Every style option this crate knows is set to a value that *changes clang-format's output* on one of the\n");
    text.push_str("sinks in `tests/options.rs`, then the same configuration is given to this formatter and the two texts are\n");
    text.push_str("compared. A `yes` means the texts are byte-identical and the case is recorded as a test.\n\n");
    text.push_str(&format!(
        "```text\noptions: {} · recorded as tests: {} · observable but different: {} · no effect on the sinks: {} · rejected values: {}\n```\n\n",
        report.len(),
        matched,
        report.iter().filter(|(_, status, _)| *status == "no").count(),
        no_effect,
        rejected
    ));
    text.push_str("`no effect` is expected for the Objective-C, Java, JavaScript, Proto, Verilog, C# and TableGen options:\n");
    text.push_str("this formatter formats C++. For the C++ options it means the sinks do not contain the construct.\n\n");
    text.push_str("| Option | Honoured | Sink, value, first difference |\n|---|---|---|\n");
    for (key, status, detail) in report {
        let detail = detail.replace('|', "\\|");
        text.push_str(&format!("| `{key}` | {status} | {detail} |\n"));
    }
    let path = workspace_root().join("docs/option-matrix.md");
    std::fs::write(path, text).expect("write the report");
}

fn workspace_root() -> PathBuf {
    // The example runs from the workspace root (`cargo run` sets the working directory there).
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

const SINK_CONSTANT: &str = r##"/// The sink the `main` cases were recorded on.
const SINK_MAIN: &str = r#"#include <vector>
#include "local.h"
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define LONG(a) \
    do { \
        (void)(a); \
    } while (0)
namespace ns {
enum class Kind { A, B };
class Widget : public Base {
public:
    Widget() : a_(0), b_(1) {}
    explicit Widget(int a) : a_(a) {}
    virtual ~Widget() = default;
    void run() override;
    int get() const { return a_; }
protected:
    int a_;
private:
    int b_;
};
template <typename T>
concept Sized = requires(T t) { t.size(); };
template <typename T, int N = 3>
struct Buffer {
    T items[N];
};
void Widget::run() {
    int x = 1;
    int *p = &x;
    const int &r = x;
    char c = 'A';
    const char *s = "hello";
    if (x > 0) {
        x = 2;
    } else {
        x = 3;
    }
    switch (x) {
    case 1:
        break;
    default:
        break;
    }
    auto f = [&](int a) -> int { return a + x; };
    std::vector<int> v = {1, 2, 3};
    int y = 0x1F + 1000 + 1500; // after a statement
    /* block */ y += 1;
    (void)p;
    (void)r;
    (void)c;
    (void)s;
    (void)f;
    (void)v;
    (void)y;
}
} // namespace ns
"#;

/// The sink the `short` cases were recorded on.
const SINK_SHORT: &str = r#"int f() { return 1; }
void g() { h(); }
void i(int x) {
    if (x) {
        h();
    }
    for (int k = 0; k < 2; ++k) {
        h();
    }
    while (x > 0) {
        --x;
    }
    do {
        --x;
    } while (x > 0);
    auto l = [](int a) { return a; };
    (void)l;
}
struct S {
    S() {}
    ~S() {}
};
"#;

/// The sink the `align` cases were recorded on.
const SINK_ALIGN: &str = r#"void f(int a, int bb, int ccc) {
    int x = 1;
    int longer = 22;
    int longest = 333;
    x = 4;
    longer = 5;
    // one
    int q = 6; // trailing
    int r = 7; // trailing
    struct T {
        int a;
        long bb;
    };
    (void)x;
    (void)longer;
    (void)q;
    (void)r;
}
"#;

/// The sink the `spacing` cases were recorded on.
const SINK_SPACING: &str = r#"void f() {
    int *p = 0;
    int &r = *p;
    int x = (int)1.5;
    char c = 'a';
    bool b = !x;
    int m = a?b:c;
    int t = 1;
    int u = -t;
    auto g = [&](int v) -> int { return v; };
    struct S {
    };
    if (b)
    {
        g(t);
    }
    (void)r;
    (void)c;
    (void)m;
    (void)u;
}
"#;

/// The sink the `preprocessor` cases were recorded on.
const SINK_PREPROCESSOR: &str = r#"#include <vector>
#include <string>
#include "local.h"
#define SHORT(a) \
    (a)
#define ALIGNED(a, b)          \
    do {                       \
        (void)(a);             \
        (void)(b);             \
    } while (0)
int f() {
#if DEBUG
    return 1;
#endif
    return 0;
}
"#;

/// The sink the `comments` cases were recorded on.
const SINK_COMMENTS: &str = r#"// A line comment that is quite long and goes on for a while so that a comment reflow has something to wrap at.
/* A block comment that is long enough for the reflow to want to change it, which it does. */
int a = 1; // one
int bb = 2; // two
int c = 3;
// own line
int d = 4; /* block */
void f() {
    // inner
    int e = 5; // inner trailing
}
"#;

"##;