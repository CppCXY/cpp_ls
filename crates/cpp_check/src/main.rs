//! **The analysis's checks, run over a whole workspace** — CI's half of the same engine an editor reads.
//!
//! ```text
//! $ cpp_check                       # the current directory, findings at `warning` and above
//! $ cpp_check E:\neovim             # a project
//! $ cpp_check --min-severity=hint   # style hints too
//! $ cpp_check --json                # machine-readable
//! $ cpp_check --code=use-after-free,dangling-return
//! ```
//!
//! # What it is, and what it is not
//!
//! It is the **same run** a reader gets in an editor: one [`Session`] over the project (the compile database, the
//! toolchain's include search, `.cppls.toml`), each file opened and read the way a compiler reads it, and
//! [`Session::diagnostics`] — so every finding here is a finding the editor would show on the same line, and a rule
//! fixed in the server is fixed here.
//!
//! It is **not** a compiler. The compiler is the authority on whether a program builds, and this tool does not
//! pretend otherwise: run it for the rules a compiler does not have — a pure virtual nothing overrides anywhere in
//! the workspace, a name nothing uses, an include that is missing, a reference to something that dies — and for the
//! files a build never sees. `docs/cpp-check.md` has the split, measured.
//!
//! # The reading step is the cost, and it is per file
//!
//! A check that resolves names against a file whose macros are unexpanded is a check about a program nobody compiles,
//! so every file is **cooked** before it is asked about: `did_open` puts the text in the VFS, `catch_up` reads its
//! summary, `cook` renders the closure and files the cooked reading `diagnostics` answers from. The three are the
//! server's own steps for a file a client opened, in the same order.
//!
//! `index_everything` looks like the batch version of that and is not: on a real project it spins — measured on
//! neovim, **seventeen seconds of CPU in sixteen minutes** with the session making no progress, the
//! "pending 0, cooking N on every round" failure its own comments describe. The server never drains that queue in one
//! call either; it pumps in slices under a budget, and so does this.
//!
//! # Why it exits non-zero
//!
//! A check run is a gate: `--min-severity` says which findings are fatal, and a run with any of them exits `1`. The
//! default (`warning`) deliberately excludes hints, because a hint is a suggestion and a build that fails on a
//! suggestion is a build somebody will turn the gate off for.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::sema::check::RuleSeverity;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

#[derive(Parser, Debug)]
#[command(
    name = "cpp_check",
    version,
    about = "Run the analysis's checks over a whole workspace.",
    long_about = None
)]
struct Args {
    /// The workspace to check. Defaults to the current directory.
    #[arg(default_value = ".")]
    root: PathBuf,

    /// Report only findings at or above this severity: hint, warning or error.
    #[arg(long, value_enum, default_value_t = Level::Warning)]
    min_severity: Level,

    /// Only these codes, by name — `--code=undefined-name,use-after-free`.
    #[arg(long, value_delimiter = ',')]
    code: Vec<String>,

    /// Skip these codes, by name.
    #[arg(long = "skip-code", value_delimiter = ',')]
    skip_code: Vec<String>,

    /// Also check headers on their own. By default only translation units are checked — a header is checked in the
    /// context of the files that include it, which is the only context it compiles in.
    #[arg(long)]
    headers: bool,

    /// Stop after this many files. A smoke run, not a check.
    #[arg(long)]
    max_files: Option<usize>,

    /// Machine-readable output: one JSON object.
    #[arg(long)]
    json: bool,

    /// Print nothing but the findings and the summary — no progress lines.
    #[arg(long)]
    quiet: bool,

    /// A path substring to skip, repeatable — `--exclude=third_party`.
    #[arg(long)]
    exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
enum Level {
    Hint,
    Warning,
    Error,
}

impl Level {
    fn rank(self) -> u8 {
        match self {
            Level::Hint => 0,
            Level::Warning => 1,
            Level::Error => 2,
        }
    }

    fn of(severity: RuleSeverity) -> Level {
        match severity {
            RuleSeverity::Error => Level::Error,
            RuleSeverity::Warning => Level::Warning,
            RuleSeverity::Hint | RuleSeverity::Information => Level::Hint,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Level::Hint => "hint",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// The extensions a translation unit is written with — C, C++ and Objective-C.
const SOURCES: &[&str] = &["c", "cc", "cpp", "cxx", "c++", "m", "mm"];

/// The extensions a header is written with.
const HEADERS: &[&str] = &[
    "h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp", "ixx", "cppm",
];

/// Directories no check run should walk into: build output, VCS data, the analysis's own cache.
const SKIPPED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".cppls",
    "target",
    "build",
    "out",
    "node_modules",
    ".vs",
    ".vscode",
    ".idea",
];

struct Report {
    file: PathBuf,
    line: usize,
    column: usize,
    level: Level,
    code: &'static str,
    message: String,
}

/// What a file's run came to — the three answers a check run can give about a file, and the reason a summary is not
/// just "findings".
#[derive(Default)]
struct Tally {
    checked: usize,
    no_reading: usize,
    unreadable: usize,
    withheld: usize,
    parse_errors: usize,
}

fn main() {
    let args = Args::parse();
    // **Not `canonicalize`**: on Windows that answers `\\?\C:\…`, and the analysis keys its index, its renderings and
    // its diagnostics on the spelling a client writes — `C:\…`. Measured here: every file "could not be read" with the
    // verbatim prefix, because `Session::diagnostics` looked for a summary under a path nobody had filed.
    let root = match std::path::absolute(&args.root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("cpp_check: cannot open {}: {error}", args.root.display());
            std::process::exit(2);
        }
    };

    let files = discover(&root, &args);
    if files.is_empty() {
        eprintln!("cpp_check: no source files under {}", root.display());
        std::process::exit(2);
    }

    // **The names the filters allow**, resolved once: an unknown code is a typo, not an empty filter, and a check run
    // that silently reports nothing because of one is worse than one that refuses.
    let only = match codes(&args.code) {
        Ok(codes) => codes,
        Err(unknown) => {
            eprintln!("cpp_check: --code names no rule: {unknown}");
            eprintln!("cpp_check: the rules are: {}", rule_names().join(", "));
            std::process::exit(2);
        }
    };
    let skipped = match codes(&args.skip_code) {
        Ok(codes) => codes,
        Err(unknown) => {
            eprintln!("cpp_check: --skip-code names no rule: {unknown}");
            eprintln!("cpp_check: the rules are: {}", rule_names().join(", "));
            std::process::exit(2);
        }
    };

    let started = Instant::now();
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.clone());

    let mut findings: Vec<Report> = Vec::new();
    let mut unreadable: Vec<PathBuf> = Vec::new();
    let mut no_reading: Vec<PathBuf> = Vec::new();
    let mut tally = Tally::default();

    for (at, file) in files.iter().enumerate() {
        if !args.quiet && !args.json && at > 0 && at % 25 == 0 {
            eprintln!(
                "cpp_check: {} / {} files, {} finding(s), {:.1} s",
                at,
                files.len(),
                findings.len(),
                started.elapsed().as_secs_f64()
            );
        }

        // **Open the file, read it in, then read it the way a compiler reads it** — the three steps a client's
        // `didOpen` takes on the server, in the same order. `catch_up` reads the file's summary; `cook` renders its
        // include closure and files the cooked reading that `diagnostics` answers from.
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(_) => {
                unreadable.push(file.clone());
                tally.unreadable += 1;
                continue;
            }
        };
        session.did_open(file, &text);
        session.catch_up(file);
        if session.cook(file).is_none() {
            // A file whose closure cannot be rendered is a file the checks have nothing trustworthy to say about. Its
            // parse errors are still the reader's business, and `diagnostics` answers from the raw reading when there
            // is no cooked one — so the file is not dropped here.
            no_reading.push(file.clone());
            tally.no_reading += 1;
        }
        // **The checks' own gate, and what it takes to open it.** A check reads *absence* out of the index — "nothing
        // declares this member" — so it may only be asked once every header this file includes unconditionally has
        // been read: otherwise an empty index would be reported as a missing declaration. `cook` renders the closure
        // but does not read every include's summary, so the pump is run here until the gate opens (bounded, because a
        // header that cannot be read at all must not stall the run).
        let mut rounds = 0usize;
        while !session.is_closure_ready(file) && rounds < 64 {
            if session.advance(256).is_empty() {
                break;
            }
            rounds += 1;
        }
        let Some(diagnostics) = session.diagnostics(file) else {
            no_reading.push(file.clone());
            tally.no_reading += 1;
            continue;
        };
        tally.checked += 1;
        tally.parse_errors += diagnostics.errors.len();
        if diagnostics.checks_withheld {
            tally.withheld += 1;
        }

        // **The file's own text, for line numbers.** A diagnostic's range is in the *rendering's* coordinates — the
        // text with macros expanded — so it is mapped back through the rendering's spans before a line is counted.
        // Measured on a file with a macro on its first line: the unmapped number was off by more than a hundred lines.
        let rendered = session.view(file);
        let in_the_file = |offset: usize| -> usize {
            rendered
                .as_ref()
                .and_then(|view| view.file_offset_of(ReadingOffset::new(offset)))
                .map(|offset| offset.get())
                .unwrap_or(offset)
        };

        for error in &diagnostics.errors {
            let start = in_the_file(error.start);
            let (line, column) = position_of(&text, start);
            findings.push(Report {
                file: file.clone(),
                line,
                column,
                level: Level::Error,
                code: "syntax-error",
                message: error.message.clone(),
            });
        }
        for finding in &diagnostics.checks {
            let level = Level::of(finding.code.severity());
            if level.rank() < args.min_severity.rank() {
                continue;
            }
            if let Some(only) = &only
                && !only.contains(finding.code.name())
            {
                continue;
            }
            if let Some(skipped) = &skipped
                && skipped.contains(finding.code.name())
            {
                continue;
            }
            let start = in_the_file(finding.range.start_offset);
            let (line, column) = position_of(&text, start);
            findings.push(Report {
                file: file.clone(),
                line,
                column,
                level,
                code: finding.code.name(),
                message: finding.message.clone(),
            });
        }
    }

    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.code.cmp(right.code))
    });

    let seconds = started.elapsed().as_secs_f64();
    let told = Fate {
        unreadable,
        no_reading,
        withheld: tally.withheld,
        parse_errors: tally.parse_errors,
        checked: tally.checked,
        seconds,
    };
    if args.json {
        report_json(&root, &files, &findings, &told);
    } else {
        report_text(&root, &files, &findings, &told);
    }

    // **The gate.** A finding at or above the minimum severity fails the run; the summary above already said what it
    // was, so the exit code carries nothing a reader has to interpret.
    let fatal = findings
        .iter()
        .filter(|finding| finding.level.rank() >= args.min_severity.rank())
        .count();
    std::process::exit(if fatal == 0 { 0 } else { 1 });
}

/// What became of the files that produced no findings of their own.
struct Fate {
    unreadable: Vec<PathBuf>,
    no_reading: Vec<PathBuf>,
    withheld: usize,
    parse_errors: usize,
    checked: usize,
    seconds: f64,
}

/// **The files to check** — the project's translation units, in a stable order.
///
/// The walk is the tool's own rather than the session's scan: the session's file list is what the *editor* owns, and it
/// is shaped by the compile database (a header is not a translation unit). A check run wants the source files a build
/// would compile, and `--headers` adds the headers that no build compiles on their own.
fn discover(root: &Path, args: &Args) -> Vec<PathBuf> {
    let mut extensions: BTreeSet<&str> = SOURCES.iter().copied().collect();
    if args.headers {
        extensions.extend(HEADERS.iter().copied());
    }

    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // **A path substring skips a subtree**, because the directories a project wants ignored are not all
                // named the same: `build`, `cmake-build-debug`, `third_party/godot-cpp`.
                if SKIPPED.iter().any(|skipped| *skipped == name)
                    || args
                        .exclude
                        .iter()
                        .any(|excluded| path.to_string_lossy().contains(excluded.as_str()))
                {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if extensions.contains(extension.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();
    if let Some(max) = args.max_files {
        files.truncate(max);
    }
    files
}

/// **The rule names a filter may use**, which is the enum's own list — a filter is a name the server also knows.
fn rule_names() -> Vec<&'static str> {
    cpp_code_analysis::DiagnosticCode::ALL
        .iter()
        .map(|code| code.name())
        .collect()
}

/// **The codes a filter names**, resolved against the enum — so `--code=undefined-nam` is a message rather than a run
/// that reports nothing. `Err` is the name nobody has.
fn codes(named: &[String]) -> Result<Option<BTreeSet<&'static str>>, String> {
    if named.is_empty() {
        return Ok(None);
    }
    let all = rule_names();
    let mut resolved = BTreeSet::new();
    for name in named {
        match all.iter().find(|known| *known == name) {
            Some(known) => {
                resolved.insert(*known);
            }
            None => return Err(name.clone()),
        }
    }
    Ok(Some(resolved))
}

/// **A byte offset as a 1-based line and column**, in the file's own text.
fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|newline| offset - newline)
        .unwrap_or(offset + 1);
    (line, column)
}

fn report_text(root: &Path, files: &[PathBuf], findings: &[Report], told: &Fate) {
    for finding in findings {
        println!(
            "{}:{}:{}  {}  {}  {}",
            relative(root, &finding.file),
            finding.line,
            finding.column,
            finding.level.text(),
            finding.code,
            finding.message
        );
    }

    // **By code, because that is what a reader acts on**: one rule with forty hits is one decision (fix it or turn it
    // off), and two rules with twenty each are two.
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in findings {
        *by_code.entry(finding.code).or_default() += 1;
    }
    if !by_code.is_empty() {
        println!("\nby code:");
        for (code, count) in by_code {
            println!("  {count:>5}  {code}");
        }
    }

    println!(
        "\n{} file(s) checked of {}, {} finding(s), {} parse error(s), {:.1} s{}",
        told.checked,
        files.len(),
        findings.len(),
        told.parse_errors,
        told.seconds,
        notes(told)
    );
}

fn report_json(root: &Path, files: &[PathBuf], findings: &[Report], told: &Fate) {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in findings {
        *by_code.entry(finding.code).or_default() += 1;
    }
    let value = serde_json::json!({
        "root": root.display().to_string(),
        "files_checked": told.checked,
        "files_seen": files.len(),
        "findings": findings.iter().map(|finding| serde_json::json!({
            "file": relative(root, &finding.file),
            "line": finding.line,
            "column": finding.column,
            "severity": finding.level.text(),
            "code": finding.code,
            "message": finding.message,
        })).collect::<Vec<_>>(),
        "by_code": by_code,
        "parse_errors": told.parse_errors,
        "unreadable": told.unreadable.iter().map(|file| relative(root, file)).collect::<Vec<_>>(),
        "no_reading": told.no_reading.iter().map(|file| relative(root, file)).collect::<Vec<_>>(),
        "checks_withheld": told.withheld,
        "seconds": told.seconds,
    });
    println!("{value}");
}

/// The sentence a run adds when something about the files themselves needs saying.
fn notes(told: &Fate) -> String {
    let mut notes = String::new();
    if !told.unreadable.is_empty() {
        notes.push_str(&format!(
            "; {} file(s) could not be read: {}",
            told.unreadable.len(),
            first_few(&told.unreadable)
        ));
    }
    if !told.no_reading.is_empty() {
        notes.push_str(&format!(
            "; {} file(s) had no reading: {}",
            told.no_reading.len(),
            first_few(&told.no_reading)
        ));
    }
    if told.withheld > 0 {
        notes.push_str(&format!(
            "; {} file(s) had their checks withheld while the project was still being read — run again for those",
            told.withheld
        ));
    }
    notes
}

fn first_few(files: &[PathBuf]) -> String {
    let mut shown = files
        .iter()
        .take(3)
        .map(|file| file.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > 3 {
        shown.push_str(", …");
    }
    shown
}

/// **A path as the run names it** — relative to the root, because a check report is read beside the project.
fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
}
#[cfg(test)]
mod tests {
    use super::*;

    /// **A byte offset as a line and column** — the arithmetic every finding's position comes from.
    #[test]
    fn a_position_is_a_line_and_a_column() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(position_of(text, 0), (1, 1), "the first byte is 1:1");
        assert_eq!(
            position_of(text, 3),
            (1, 4),
            "the newline is the end of line one"
        );
        assert_eq!(
            position_of(text, 4),
            (2, 1),
            "the byte after it is the start of two"
        );
        assert_eq!(
            position_of(text, 8),
            (3, 1),
            "and three starts after the second"
        );
        // **A column past the end is clamped rather than panicking** — a diagnostic can point at the end of the text,
        // and a slicing panic there would lose the whole run rather than one number.
        assert_eq!(
            position_of(text, 999),
            (4, 1),
            "past the end is the empty last line"
        );
    }

    /// **The walk finds sources and skips what a build does not own** — the file list is the run's subject.
    #[test]
    fn the_walk_takes_sources_and_skips_build_output() {
        let root = std::env::temp_dir().join(format!("cpp_check_walk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("a temp dir");
        std::fs::create_dir_all(root.join("build")).expect("a temp dir");
        for (path, text) in [
            ("src/a.cpp", "int a;\n"),
            ("src/b.c", "int b;\n"),
            ("src/c.h", "int c;\n"),
            ("src/d.hpp", "int d;\n"),
            ("build/generated.cpp", "int e;\n"),
        ] {
            std::fs::write(root.join(path), text).expect("a fixture file");
        }

        let sources = discover(
            &root,
            &Args {
                root: root.clone(),
                min_severity: Level::Warning,
                code: Vec::new(),
                skip_code: Vec::new(),
                headers: false,
                max_files: None,
                json: false,
                quiet: true,
                exclude: Vec::new(),
            },
        );
        let named: Vec<String> = sources
            .iter()
            .map(|path| relative(&root, path).replace('\\', "/"))
            .collect();
        assert_eq!(
            named,
            vec!["src/a.cpp", "src/b.c"],
            "sources, in path order, with `build/` left out and headers excluded"
        );

        // **`--headers` adds the headers, and only the headers' extensions** — `c.h` and `d.hpp` are checked on their
        // own, which is the one context a build never gives them.
        let with_headers = discover(
            &root,
            &Args {
                root: root.clone(),
                min_severity: Level::Warning,
                code: Vec::new(),
                skip_code: Vec::new(),
                headers: true,
                max_files: None,
                json: false,
                quiet: true,
                exclude: Vec::new(),
            },
        );
        let named: Vec<String> = with_headers
            .iter()
            .map(|path| relative(&root, path).replace('\\', "/"))
            .collect();
        assert_eq!(
            named,
            vec!["src/a.cpp", "src/b.c", "src/c.h", "src/d.hpp"],
            "every source and header, still without `build/`"
        );

        // **`--exclude` keeps a subtree out by substring**, which is how a project names its vendored code.
        let excluded = discover(
            &root,
            &Args {
                root: root.clone(),
                min_severity: Level::Warning,
                code: Vec::new(),
                skip_code: Vec::new(),
                headers: false,
                max_files: None,
                json: false,
                quiet: true,
                exclude: vec!["src".to_string()],
            },
        );
        assert!(excluded.is_empty(), "everything under `src` was excluded");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A rule name is a name the server also knows**, and a name nobody has is an error rather than an empty filter.
    #[test]
    fn a_code_filter_resolves_against_the_rules() {
        let known = codes(&["undefined-name".to_string(), "use-after-free".to_string()])
            .expect("both are rules");
        let known = known.expect("a filter");
        assert!(known.contains("undefined-name"));
        assert!(known.contains("use-after-free"));

        assert_eq!(
            codes(&[]).expect("no filter"),
            None,
            "no names is no filter"
        );

        let unknown = codes(&["undefined-nam".to_string()]);
        assert_eq!(
            unknown.expect_err("a typo"),
            "undefined-nam",
            "the name nobody has is named back"
        );
    }
}
