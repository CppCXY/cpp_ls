//! **A measurement of the C++ corpus** — one line per file, one line per member question, one line per type question.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test cpp_corpus -- --ignored --nocapture
//! $env:CPPLS_CPP_CORPUS = "C:\Users\zc\Desktop\cpp_project"   # the default
//! ```
//!
//! The corpus is a real project: `CMakeLists.txt` builds it with MinGW's g++ 15 in C++23 mode, and
//! `build/compile_commands.json` carries `-std=c++23`. Every construct in it is one the compiler accepted, which is
//! what makes a diagnostic here a defect of the analysis rather than of the file.
//!
//! # The three questions
//!
//! ```text
//! parse     does each file read without a syntax error, and how many declarations did it produce?
//! members   for each `x.member` / `x->member` a reader is likely to ask about, is `member` in the list the editor shows?
//! types     for each declaration, is the declared name's type the one the file wrote?
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn corpus_root() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_CPP_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp_project"));
    named.join("main.cpp").is_file().then_some(named)
}

/// **One member question**: the file, the expression whose operator the cursor goes after, and the member expected.
struct MemberRow {
    file: &'static str,
    needle: &'static str,
    expected: &'static str,
}

/// **One type question**: the file, a piece of text that contains the declared name, the name itself, and what the
/// type is expected to contain.
struct TypeRow {
    file: &'static str,
    needle: &'static str,
    name: &'static str,
    expected: &'static str,
}

const MEMBERS: &[MemberRow] = &[
    // **SFINAE and the detection idiom**: the class template`'s own members, and a partially specialised one.
    MemberRow {
        file: "src/techniques.cpp",
        needle: "box.get()",
        expected: "get",
    },
    MemberRow {
        file: "src/techniques.cpp",
        needle: "box.set(9)",
        expected: "set",
    },
    MemberRow {
        file: "src/techniques.cpp",
        needle: "first.self()",
        expected: "self",
    },
    // The deduced `Box box(7)`: the deduction guide is what gives it a type at all.
    MemberRow {
        file: "src/techniques.cpp",
        needle: "box.get()",
        expected: "get",
    },
    // A standard container, through the template function that deduced it.
    MemberRow {
        file: "src/techniques.cpp",
        needle: "numbers.front()",
        expected: "front",
    },
    // **The shapes a reader meets next** — `std::optional`'s own members, and a `std::function` called.
    MemberRow {
        file: "src/techniques.cpp",
        needle: "maybe.has_value()",
        expected: "has_value",
    },
    MemberRow {
        file: "src/techniques.cpp",
        needle: "maybe->size()",
        expected: "size",
    },
    MemberRow {
        file: "src/techniques.cpp",
        needle: "number.value_or(0)",
        expected: "value_or",
    },
    MemberRow {
        file: "src/techniques.cpp",
        needle: "maybe.value()",
        expected: "value",
    },
    // **An inherited member**, reached through the instance of the derived class.
    MemberRow {
        file: "src/techniques.cpp",
        needle: "derived.base_value",
        expected: "base_value",
    },
];

const TYPES: &[TypeRow] = &[
    TypeRow {
        file: "src/techniques.cpp",
        needle: "techniques::Box<int> box",
        name: "box",
        expected: "Box<int>",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "const std::vector<int> numbers",
        name: "numbers",
        expected: "vector<int>",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "techniques::Box box(7)",
        name: "box",
        expected: "Box<int>",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "Widget widget;",
        name: "widget",
        expected: "Widget",
    },
    // The two `std::optional` declarations: what the *name* is typed as, before any member question.
    TypeRow {
        file: "src/techniques.cpp",
        needle: "std::optional<std::string> maybe",
        name: "maybe",
        expected: "optional",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "std::optional<int> number",
        name: "number",
        expected: "optional",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "std::function<int(int, int)> operation",
        name: "operation",
        expected: "function",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto copied = box.copy_of()",
        name: "copied",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto doubled = box.doubled()",
        name: "doubled",
        expected: "int",
    },
    // **A fold expression over a pack**: `(values + ...)` of four `int`s is an `int`, and of `int + double` a `double`.
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto total = techniques::sum_of(1, 2, 3, 4)",
        name: "total",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto mixed = techniques::sum_of(1, 2.5)",
        name: "mixed",
        expected: "double",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto [number, fraction] = pair",
        name: "number",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto result = doubler(3)",
        name: "result",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto found = counts[\"a\"]",
        name: "found",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto widened = true ? 1 : 2.0",
        name: "widened",
        expected: "double",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto allocated = new Closing{}",
        name: "allocated",
        expected: "Closing*",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "for (const auto& [key, value] : table)",
        name: "value",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "decltype(auto) same = widen_gently(1)",
        name: "same",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto through_pointer = fp(3)",
        name: "through_pointer",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto cut = text.substr(1, 2)",
        name: "cut",
        // **The instantiation, not the alias**: `substr` is declared to return `basic_string`, the injected-class-name,
        // so the answer is the class with the receiver's own arguments.
        expected: "basic_string<char",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto shared = Holder::instances",
        name: "shared",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto nine = operation(3, 6)",
        name: "nine",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto held = std::get<int>(either)",
        name: "held",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "constexpr int kMax = 10",
        name: "kMax",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto from_constant = doubled_constant(kMax)",
        name: "from_constant",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "constinit static int counter = 0",
        name: "counter",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "Count count = 3",
        name: "count",
        // **The alias name is the answer**: a C++ toolchain shows `Count` here, because that is the spelling the
        // declaration used and the name the reader wrote.
        expected: "Count",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "Fraction fraction = 0.5",
        name: "fraction",
        expected: "Fraction",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto colour = Colour::Red",
        name: "colour",
        expected: "Colour",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto bytes = sizeof(int)",
        name: "bytes",
        expected: "size_t",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto printed = std::to_string(count)",
        name: "printed",
        expected: "string",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "Outer::Inner nested",
        name: "nested",
        expected: "Inner",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto owned = std::make_unique<Closing>()",
        name: "owned",
        expected: "unique_ptr",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto described = derived.describe()",
        name: "described",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto sum = a + b",
        name: "sum",
        expected: "Money",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto same = (a == b)",
        name: "same",
        expected: "bool",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto one_pair = pairs.front()",
        name: "one_pair",
        expected: "pair<int, double>",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto raw = widget.raw()",
        name: "raw",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto stored = the_widget().stored",
        name: "stored",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto first = pair.first",
        name: "first",
        expected: "int",
    },
    TypeRow {
        file: "src/techniques.cpp",
        needle: "auto second = pair.second",
        name: "second",
        expected: "double",
    }
];

fn corpus_files(root: &Path) -> Vec<PathBuf> {
    let opened = Session::open(
        root.to_path_buf(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(root),
    );
    let mut files: Vec<PathBuf> = opened
        .project_files()
        .iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("cpp" | "h")
            )
        })
        .filter(|path| !path.components().any(|part| part.as_os_str() == "build"))
        .cloned()
        .collect();
    files.sort();
    files
}

fn name_of(path: &Path) -> String {
    path.strip_prefix(path.ancestors().nth(2).unwrap_or(path))
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn member_labels(session: &Session<DiskFiles>, path: &Path, cursor: usize) -> Vec<String> {
    let Some(view) = session.view(path) else {
        return Vec::new();
    };
    session
        .completions(&view, FileOffset::new(cursor))
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn declared_type(session: &Session<DiskFiles>, path: &Path, cursor: usize) -> Option<String> {
    let view = session.view(path)?;
    let reading = view.reading_offset_of(FileOffset::new(cursor))?;
    match session.type_at(&view, reading) {
        Known::Yes(found) => Some(found.type_of.to_string()),
        // **An unknown is not a type**: the probe's pass rule is "the answer contains the expected spelling", and
        // `<unknown: `std::get<int>` is declared more than once…>` contains `int` — so an unknown answer passed a row that
        // asked for `int`. Measured: it did exactly that.
        Known::Unknown(_) => None,
        Known::No => None,
    }
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_cpp_corpus_parses_and_its_members_resolve() {
    let Some(root) = corpus_root() else {
        eprintln!("skipped: no corpus (set CPPLS_CPP_CORPUS to the cpp_project project)");
        return;
    };
    let files = corpus_files(&root);
    println!(
        "\n=== the corpus: {} file(s) under {}",
        files.len(),
        root.display()
    );

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.clone());
    let indexing = std::time::Instant::now();
    session.index_everything();
    let mut steps = 0usize;
    while session.pending_work() > 0 && steps < 20_000 {
        session.advance(64);
        steps += 1;
    }
    println!("indexed in {:?} ({steps} step(s))", indexing.elapsed());

    println!("\n--- parse: one line per file");
    let mut files_with_errors = 0usize;
    let mut total_errors = 0usize;
    let mut declarations = 0usize;
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            println!("  {:<30} unreadable", name_of(path));
            continue;
        };
        let started = std::time::Instant::now();
        let cooked = session.cook(path).is_some();
        let cooking = started.elapsed();
        let found = session.diagnostics(path);
        let errors = found.as_ref().map(|found| found.errors.len()).unwrap_or(0);
        let checks = found.as_ref().map(|found| found.checks.len()).unwrap_or(0);
        total_errors += errors;
        if errors > 0 {
            files_with_errors += 1;
        }
        let bound = session
            .parsed_summary(path)
            .map(|summary| summary.declarations.len())
            .unwrap_or(0);
        declarations += bound;
        println!(
            "  {:<30} {:>3} error(s)  {:>3} finding(s)  {:>5} decl  {:>5} ms  {:>4} line(s){}",
            name_of(path),
            errors,
            checks,
            bound,
            cooking.as_millis(),
            text.lines().count(),
            if cooked { "" } else { "  NOT COOKED" }
        );
        if let Some(found) = &found {
            let mut by_code: BTreeMap<&str, Vec<&cpp_code_analysis::Finding>> = BTreeMap::new();
            for check in &found.checks {
                by_code.entry(check.code.name()).or_default().push(check);
            }
            let all = std::env::var_os("CPPLS_C_ALL").is_some();
            for (code, entries) in by_code {
                println!("      [{code}] {} finding(s)", entries.len());
                let shown = if all { entries.len() } else { 3 };
                for check in entries.iter().take(shown) {
                    let from = check.range.start_offset.saturating_sub(48).min(text.len());
                    println!(
                        "        …{}«{}»…  ← {}",
                        text[from..check.range.start_offset].replace('\n', " "),
                        text[check.range.start_offset..check.range.end_offset().min(text.len())]
                            .replace('\n', " "),
                        check.message.chars().take(80).collect::<String>()
                    );
                }
            }
            for error in found.errors.iter().take(6) {
                // **The line, not a byte window.** A parse error's offset can sit on a newline or at the end of the
                // token that failed, and a window then shows nothing around it; the line the offset is on is what a
                // reader can point at.
                let at = error.start.min(text.len());
                let line = text[..at].bytes().filter(|byte| *byte == b'\n').count() + 1;
                let from = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
                let to = text[at..]
                    .find('\n')
                    .map_or(text.len(), |newline| at + newline);
                println!(
                    "      parse: line {line}: {}  ← {}",
                    text[from..to].trim(),
                    error.message.chars().take(90).collect::<String>()
                );
            }
        }
    }
    println!(
        "  totals: {} file(s), {} with a syntax error, {} error(s), {} declaration(s)",
        files.len(),
        files_with_errors,
        total_errors,
        declarations
    );

    for path in &files {
        if session.view(path).is_none() && session.cook(path).is_none() {
            println!("  WARN  {} has no reading to ask about", path.display());
        }
    }

    println!("\n--- members: the completion at the position right after `.` / `->`");
    let mut member_failures = 0usize;
    for row in MEMBERS {
        let path = root.join(row.file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("  SKIP  {:<24} {} (unreadable)", row.file, row.needle);
            continue;
        };
        let Some(found_at) = text.find(row.needle) else {
            println!("  SKIP  {:<24} {} (not written)", row.file, row.needle);
            member_failures += 1;
            continue;
        };
        let operator_at = row
            .needle
            .find("->")
            .or_else(|| row.needle.find('.'))
            .expect("every member row names an operator");
        let width = if row.needle[operator_at..].starts_with("->") {
            2
        } else {
            1
        };
        let cursor = found_at + operator_at + width;

        if session.view(&path).is_none() {
            session.cook(&path);
        }
        let labels = member_labels(&session, &path, cursor);
        let pass = labels.iter().any(|label| label == row.expected);
        if !pass {
            member_failures += 1;
        }
        println!(
            "  {:<4}  {:<28} {:<26} {:>3} offered  {}",
            if pass { "PASS" } else { "FAIL" },
            row.file,
            row.needle,
            labels.len(),
            if pass {
                String::new()
            } else if labels.is_empty() {
                format!("nothing offered — `{}` is not reachable", row.expected)
            } else {
                format!(
                    "`{}` missing from {:?}",
                    row.expected,
                    labels.iter().take(10).collect::<Vec<_>>()
                )
            }
        );
    }

    println!("\n--- types: `type_at` with the cursor inside the declared name");
    let mut type_failures = 0usize;
    for row in TYPES {
        let path = root.join(row.file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("  SKIP  {:<24} {}", row.file, row.needle);
            continue;
        };
        let Some(found_at) = text.find(row.needle) else {
            println!("  SKIP  {:<24} {} (not written)", row.file, row.needle);
            type_failures += 1;
            continue;
        };
        let Some(name_at) = row.needle.find(row.name) else {
            println!(
                "  SKIP  {:<24} {} (the name is not in the needle)",
                row.file, row.needle
            );
            continue;
        };
        let cursor = found_at + name_at + 1;
        if session.view(&path).is_none() {
            session.cook(&path);
        }
        let answer = declared_type(&session, &path, cursor);
        let pass = answer
            .as_deref()
            .is_some_and(|answ| answ.contains(row.expected));
        if !pass {
            type_failures += 1;
        }
        println!(
            "  {:<4}  {:<28} {:<20} → {}",
            if pass { "PASS" } else { "FAIL" },
            row.file,
            row.name,
            answer.unwrap_or_else(|| "<no type>".to_string())
        );
    }

    if std::env::var_os("CPPLS_C_MEMBERS").is_some()
        && let Some(view) = session.view(root.join("src/strings.cpp"))
    {
        println!(
            "\n[members] config standard: {:?}",
            session.config().standard
        );
        for spelling in ["std::string_view", "std::string", "std::future"] {
            match session.members_of(&view, spelling) {
                Known::Yes(list) => println!(
                    "\n[members] {spelling}: {} member(s), unlisted={}, has starts_with={}",
                    list.members.len(),
                    list.unlisted.len(),
                    list.members
                        .iter()
                        .any(|member| member.fact.name == "starts_with")
                ),
                Known::Unknown(reason) => println!("\n[members] {spelling}: unknown ({reason:?})"),
                Known::No => println!("\n[members] {spelling}: no"),
            }
        }
    }

    println!(
        "\n=== summary: {} file(s) ({} with errors), {}/{} members, {}/{} types",
        files.len(),
        files_with_errors,
        MEMBERS.len() - member_failures,
        MEMBERS.len(),
        TYPES.len() - type_failures,
        TYPES.len()
    );
}
