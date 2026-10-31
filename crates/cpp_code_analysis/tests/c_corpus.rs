//! **A measurement of the C corpus** — one line per file, one line per member question, one line per type question.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test c_corpus -- --ignored --nocapture
//! $env:CPPLS_C_CORPUS = "C:\Users\zc\Desktop\c-project"   # the default
//! ```
//!
//! The corpus is a real project: `CMakeLists.txt` builds it with MinGW's gcc 15 in C23 mode,
//! `build/compile_commands.json` carries `-std=gnu2x`, and every construct in it is one the compiler accepted. That is
//! what makes the numbers here mean something — a file that compiles and that this analysis reports an error in is a
//! defect of the analysis, not of the file.
//!
//! # What the corpus is for
//!
//! Two defects, and only those two (see the corpus's own `README.md`):
//!
//! * **a class-body member whose specifier starts with `_BitInt(`** is dropped from the summary — `three` and `other`
//!   are recorded (their specifier starts with `unsigned`), `signed_wide` and `plain` are not;
//! * **a function-pointer typedef declared in a header**, used for a local variable, answers `<no type>`, while the same
//!   shape in a two-file fixture answers `int (*)(int, int)`.
//!
//! # The three questions
//!
//! ```text
//! parse     does each file read without a syntax error, and how many declarations did it produce?
//! members   for each `x.member` / `x->member` a reader is likely to ask about, is `member` in the list the editor shows?
//! types     for each declaration, is the declared name's type the one the file wrote?
//! ```
//!
//! **Members are asked through the completion**, at the position right after the `.`/`->` — the same request an
//! editor sends, so an answer here is an answer a reader gets. `types` is asked through [`Session::type_at`] with the
//! cursor inside the declared name, which is the hover's question.
//!
//! Nothing is asserted: this prints, and a `FAIL` line is the work list. The three kinds of failure mean different
//! things, and the line says which: a parse error is the grammar, a missing member is the index or the type, and a
//! missing type is the declarator.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn corpus_root() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_C_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\c-project"));
    named
        .join("include")
        .join("corpus.h")
        .is_file()
        .then_some(named)
}

/// **One member question**: the file, the expression whose operator the cursor goes after, and the member expected.
///
/// `needle` is the whole expression as the file writes it — `local.tm_year`, `pointer->tm_year` — and the cursor is
/// placed **immediately after the operator**, which is the position a reader is in when the popup opens.
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

/// **The corpus was consolidated** — `src/bits.c` was merged into `src/c23.c`, so these rows name the shape where
/// it now lives. `bits.three` and `bits.signed_wide` ask the two bit-fields of one anonymous struct: the first is
/// written `unsigned _BitInt(3)`, the second `_BitInt(13)`, and the difference between them is the defect `§N16`
/// describes — a member whose specifier *begins* with `_BitInt(` is dropped.
const MEMBERS: &[MemberRow] = &[
    MemberRow {
        file: "src/c23.c",
        needle: "bits.three",
        expected: "three",
    },
    MemberRow {
        file: "src/c23.c",
        needle: "bits.signed_wide",
        expected: "signed_wide",
    },
];

/// The type questions: the struct variable, the two locals whose type is a function-pointer typedef from a header,
/// and the alias spelling itself.
///
/// **The two are different questions, and measuring one as the other is a mistake this file made once.** A cursor
/// inside the *declarator's name* (`operation`) asks what the file said the variable is — `binary_op`, the alias's
/// name, which is the spelling a hover shows. A cursor inside the *type's name* (`binary_op`) asks what the alias
/// denotes — `int (*)(int, int)` — and that is what the two-file fixture in `tests/types.rs` asserts. Both answers are
/// right; the old rows here asked the first question and expected the second answer, so they reported a defect that
/// did not exist.
const TYPES: &[TypeRow] = &[
    TypeRow {
        file: "src/functions.c",
        needle: "binary_op operation",
        name: "operation",
        // The variable's type is what the file wrote: the alias's name.
        expected: "binary_op",
    },
    TypeRow {
        file: "src/functions.c",
        needle: "comparer *compare",
        name: "compare",
        // A function-type alias: the variable is a pointer to the function, and the file wrote the alias.
        expected: "comparer",
    },
    TypeRow {
        file: "src/functions.c",
        needle: "binary_op operation",
        name: "binary_op",
        // The alias itself denotes the signature, and the engine expands it — the question the two-file fixture asks.
        expected: "(int, int)",
    },
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
                Some("c" | "h")
            )
        })
        .filter(|path| !path.components().any(|part| part.as_os_str() == "build"))
        .cloned()
        .collect();
    files.sort();
    files
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_c_corpus_parses_and_its_members_resolve() {
    let Some(root) = corpus_root() else {
        eprintln!(
            "skipped: no corpus (set CPPLS_C_CORPUS to a project whose include/corpus.h exists)"
        );
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
    let indexed = drain(&mut session);
    println!(
        "indexed in {:?} ({indexed} step(s) of work)",
        indexing.elapsed()
    );

    // **`Session::cook` is the synchronous cook**, and it is the right one here: a file's rendering inlines its whole
    // include closure, so the work has to be seen to happen, once per file. The queue-driven path
    // (`want_cooked_reading` + `advance`) does **not** cook — `advance` pops the *indexing* queue — which is what made
    // the first version of this probe spin and answer nothing.
    let _ = drain(&mut session);

    // ---------------------------------------------------------------------------------------------------------
    // parse
    // ---------------------------------------------------------------------------------------------------------
    println!("\n--- parse: one line per file, in the order the corpus lists them");
    let mut files_with_errors = 0usize;
    let mut total_errors = 0usize;
    let mut declarations = 0usize;
    for path in &files {
        let started = std::time::Instant::now();
        let Ok(text) = std::fs::read_to_string(path) else {
            println!("  {:<28} unreadable", name_of(path));
            continue;
        };
        let cooking = std::time::Instant::now();
        let cooked = session.cook(path).is_some();
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
            "  {:<28} {:>3} error(s)  {:>3} finding(s)  {:>4} decl  {:>5} ms cook {:>5} ms  {} line(s){}",
            name_of(path),
            errors,
            checks,
            bound,
            started.elapsed().as_millis(),
            cooking.elapsed().as_millis(),
            text.lines().count(),
            if cooked { "" } else { "  NOT COOKED" }
        );
        if let Some(found) = &found {
            // **A window around the offset in the file.** Every range the diagnostics channel reports has been mapped
            // into the file — `add_in_the_file` for a rule that reads the summary, `map_into_the_file` for one that
            // reads the rendering — so the file is the text an offset is into.
            let where_at = |offset: usize| -> String {
                let text = &text;
                let from = offset.saturating_sub(64).min(text.len());
                let to = (offset + 64).min(text.len());
                let before = text[..from]
                    .char_indices()
                    .next_back()
                    .map_or(from, |(at, _)| at);
                let after = text[to..]
                    .char_indices()
                    .next()
                    .map_or(text.len(), |(at, _)| to + at);
                format!(
                    "…{}«{}»{}…",
                    text[before..offset.min(after)].trim_end(),
                    text[offset.min(after)..offset.min(after).max(offset.min(after))].trim(),
                    text[offset.min(after)..after].trim_start()
                )
            };
            for error in found.errors.iter().take(6) {
                println!(
                    "      line {}: {}  ← {}",
                    line_of(&text, error.start),
                    where_at(error.start),
                    error.message.chars().take(90).collect::<String>()
                );
            }
            // **Every finding, grouped by code** — three samples per file is how the first version of this probe hid
            // that one code accounted for all of them. `CPPLS_C_ALL` lifts the cap.
            let mut by_code: BTreeMap<&str, Vec<&cpp_code_analysis::Finding>> = BTreeMap::new();
            for check in &found.checks {
                by_code.entry(check.code.name()).or_default().push(check);
            }
            let shown_when_capped = 3;
            let all = std::env::var_os("CPPLS_C_ALL").is_some();
            for (code, entries) in by_code {
                println!("      [{code}] {} finding(s)", entries.len());
                let shown = if all {
                    entries.len()
                } else {
                    shown_when_capped
                };
                for check in entries.iter().take(shown) {
                    println!(
                        "        line {}: {}  ← {}",
                        line_of(&text, check.range.start_offset),
                        where_at(check.range.start_offset),
                        check.message.chars().take(80).collect::<String>()
                    );
                }
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

    dump_names(&session, &root);

    // **Re-cook the files whose rendering the cache has dropped.** The rendering cache is bounded by weight, so a file
    // cooked early is evicted by the ones after it, and `Session::view` then answers `None` — which the rows below
    // read as "the engine offered nothing". Measured on the previous corpus: two files answered `0 offered` for every
    // row while the last one answered all of them. A cook is idempotent.
    for path in &files {
        if session.view(path).is_none() && session.cook(path).is_none() {
            println!("  WARN  {} has no reading to ask about", path.display());
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // macros: the answers an editor asks for at the four places a reader points at
    // ---------------------------------------------------------------------------------------------------------
    println!("\n--- macros: what the three queries answer at each place a reader points at");
    {
        // The offsets are the **file's**, which is the coordinate a client sends.
        // **(file, the line to find, the word on it the cursor is on)** — the word matters: a directive's first word
        // is `ifdef` or `define`, and a row that pointed there measured the keyword rather than the macro.
        for (file, needle, word) in [
            // A predefined macro in a directive's condition.
            (
                "src/preprocessor.c",
                "#ifdef __STDC_VERSION__",
                "__STDC_VERSION__",
            ),
            // A macro at its own definition.
            (
                "src/preprocessor.c",
                "#define CORPUS_STANDARD __STDC_VERSION__",
                "CORPUS_STANDARD",
            ),
            // …and the body of that same definition.
            (
                "src/preprocessor.c",
                "#define CORPUS_STANDARD __STDC_VERSION__",
                "__STDC_VERSION__",
            ),
            // A macro use inside an enum list — the X-macro a reader points at.
            (
                "src/preprocessor.c",
                "CORPUS_COLOURS(CORPUS_ENUM_ENTRY)",
                "CORPUS_COLOURS",
            ),
            // …and the inner name of that X-macro's row.
            (
                "src/preprocessor.c",
                "CORPUS_COLOURS(CORPUS_ENUM_ENTRY)",
                "CORPUS_ENUM_ENTRY",
            ),
            // A C11 keyword construct.
            (
                "src/preprocessor.c",
                "_Static_assert(sizeof(point)",
                "_Static_assert",
            ),
            // A variadic macro's use.
            ("src/c23.c", "C23_SUM(count, 1, 2, 3)", "C23_SUM"),
        ] {
            let path = root.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(found_at) = text.find(needle) else {
                println!("  SKIP  {file} {needle} (not written)");
                continue;
            };
            let word_at = found_at + needle.find(word).expect("the word is on the line");
            let Some(view) = session.view(&path) else {
                println!("  SKIP  {file} (no reading)");
                continue;
            };
            let file_offset = FileOffset::new(word_at);
            let by_file = session
                .macro_at_a_file_offset(&view, file_offset)
                .value()
                .map(|found| {
                    format!(
                        "{}:{}",
                        found.file.file_name().unwrap_or_default().to_string_lossy(),
                        found.fact.range.start_offset
                    )
                });
            let by_rendering = view.reading_offset_of(file_offset).and_then(|offset| {
                session
                    .macro_definition(&view, offset)
                    .value()
                    .map(|found| found.fact.name.clone())
            });
            let goto = view
                .reading_offset_of(file_offset)
                .and_then(|offset| {
                    session
                        .definition(&view, offset)
                        .value()
                        .map(|found| found.file.clone())
                })
                .map(|file| {
                    file.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                });
            println!(
                "  {:<18} {:<40} hover/name {:?}  rendering {:?}  goto {:?}",
                file,
                needle.chars().take(38).collect::<String>(),
                by_file,
                by_rendering,
                goto
            );
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // completions: what a cursor inside this file's own code is offered
    // ---------------------------------------------------------------------------------------------------------
    println!("\n--- completions: does the file's own content come back?");
    {
        for (file, needle, wanted) in [
            // Inside the function that uses the macro, at the start of a statement.
            ("src/c23.c", "return C23_SUM(", "C23_SUM"),
            ("src/c23.c", "return C23_SUM(", "c23_sum_impl"),
            // At a blank position inside a function body — the ordinary "what is in scope here".
            ("src/c23.c", "int c23_va_opt(", "c23_sum_impl"),
            ("src/functions.c", "int total = operation", "operation"),
            ("src/types.c", "int main", "CORPUS_"),
            // The last function of `functions.c`, where a reader reported that `fprintf` never completes.
            ("src/functions.c", "fprintf(", "fprintf"),
            // **Simulating what a reader types**: the cursor *inside* the word, so the prefix is non-empty — which is
            // the case the report is about. A completion at `(` answers "everything visible", not "what I typed".
            ("src/functions.c", "fpr", "fprintf"),
            ("src/functions.c", "fprint", "fprintf"),
            ("src/functions.c", "fp", "fprintf"),
            ("src/functions.c", "fprintf(stderr", "fprintf"),
            ("src/functions.c", "\"aborting with %d\\\\n\"", "fprintf"),
        ] {
            let path = root.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(found_at) = text.find(needle) else {
                println!("  SKIP  {file} {needle} (not written)");
                continue;
            };
            // The cursor goes **after** the needle: inside the call's arguments, or just after the `(`.
            let cursor = found_at + needle.len();
            let Some(view) = session.view(&path) else {
                println!("  SKIP  {file} (no reading)");
                continue;
            };
            let started = std::time::Instant::now();
            let found = session.completions(&view, FileOffset::new(cursor));
            let spent = started.elapsed().as_millis();
            let labels: Vec<String> = found.items.iter().map(|item| item.label.clone()).collect();
            let has = labels.iter().any(|label| label == wanted);
            println!(
                "  {:<18} {:?} → {} item(s), {wanted} {}, {spent} ms",
                file,
                needle,
                labels.len(),
                if has { "✓" } else { "✗" }
            );
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // members
    // ---------------------------------------------------------------------------------------------------------
    println!("\n--- members: the completion at the position right after `.` / `->`");
    let mut member_failures = 0usize;
    let mut member_by_file: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for row in MEMBERS {
        let path = root.join(row.file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("  SKIP  {:<24} {} (unreadable)", row.file, row.needle);
            continue;
        };
        let Some(found_at) = text.find(row.needle) else {
            println!("  SKIP  {:<24} {} (not written)", row.file, row.needle);
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

        // **A file's rendering can be evicted while the loop moves on**, and then every row about it answers `0` — see
        // the note on the pre-pass above. Cooking it again here is the only way to ask the *engine* rather than the
        // cache: the cook is a cache hit when the rendering is still held, and a rebuild when it is not.
        if session.view(&path).is_none() {
            session.cook(&path);
        }
        let labels = member_labels(&session, &path, cursor);
        if labels.is_empty() && std::env::var_os("CPPLS_TRACE_MEMBERS").is_some() {
            match session.view(&path) {
                Some(view) => println!(
                    "  trace: {} rendering {} chars, reading offset {:?}",
                    row.file,
                    view.source.len(),
                    view.reading_offset_of(FileOffset::new(cursor))
                ),
                None => println!("  trace: {} no view", row.file),
            }
        }
        let pass = labels.iter().any(|label| label == row.expected);
        let entry = member_by_file.entry(row.file).or_default();
        entry.0 += 1;
        if pass {
            entry.1 += 1;
        } else {
            member_failures += 1;
        }
        println!(
            "  {:<4}  {:<24} {:<26} {:>3} offered  {}",
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
                    labels.iter().take(8).collect::<Vec<_>>()
                )
            }
        );
    }
    let member_asked: usize = member_by_file.values().map(|(asked, _)| asked).sum();
    let member_resolved: usize = member_by_file.values().map(|(_, resolved)| resolved).sum();
    println!(
        "  totals: {member_resolved}/{member_asked} member question(s) answered, {member_failures} missing"
    );
    for (file, (asked, resolved)) in &member_by_file {
        println!("    {:<26} {resolved}/{asked}", file);
    }

    // ---------------------------------------------------------------------------------------------------------
    // types
    // ---------------------------------------------------------------------------------------------------------
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
        let answer = declared_type(&session, &path, cursor);
        let pass = answer
            .as_deref()
            .is_some_and(|answ| answ.contains(row.expected));
        if !pass {
            type_failures += 1;
        }
        println!(
            "  {:<4}  {:<24} {:<26} → {}",
            if pass { "PASS" } else { "FAIL" },
            row.file,
            row.name,
            match &answer {
                Some(answer) => answer.clone(),
                None => "<no type>".to_string(),
            }
        );
    }
    println!(
        "  totals: {}/{type_asked} type question(s) answered, {type_failures} wrong",
        TYPES.len() - type_failures,
        type_asked = TYPES.len()
    );

    println!(
        "\n=== summary: {} file(s) ({} with errors), {member_resolved}/{member_asked} members, {}/{} types",
        files.len(),
        files_with_errors,
        TYPES.len() - type_failures,
        TYPES.len()
    );
}

/// **Drain the session's work queue, with a bound.** A queue that never empties must not become an infinite loop in a
/// probe: the first version of this file spun for twenty minutes because `pending_work` kept saying there was more.
fn drain(session: &mut Session<DiskFiles>) -> usize {
    let mut steps = 0usize;
    while session.pending_work() > 0 && steps < 20_000 {
        session.advance(64);
        steps += 1;
    }
    if session.pending_work() > 0 {
        eprintln!(
            "warning: the queue still holds {} work after {steps} step(s)",
            session.pending_work()
        );
    }
    steps
}

/// **What the index actually holds for a name** — the question every `undefined-name` finding turns into.
///
/// `CPPLS_C_NAMES="bits,signed_wide"` prints every fact filed under each name, with the file it came from, its kind,
/// its type and its parameter list. A name with no fact at all is a declaration the summary did not record; a fact
/// with an empty type is a declaration it recorded but could not read.
///
/// `CPPLS_C_FACTS="src/bits.c"` prints **every** fact of one file, which is the question a name cannot answer: whether
/// a declaration was recorded at all, and what type the summary gave it.
fn dump_names(session: &Session<DiskFiles>, root: &Path) {
    if let Ok(file) = std::env::var("CPPLS_C_FACTS") {
        let path = root.join(&file);
        println!("\n--- every declaration of {file}");
        if let Some(summary) = session.parsed_summary(&path) {
            for fact in &summary.declarations {
                println!(
                    "  {:<28} {:<12} type {:<28} local {} scope {:?}",
                    fact.name,
                    format!("{:?}", fact.kind),
                    fact.type_of.as_deref().unwrap_or("<none>"),
                    fact.local,
                    fact.scope
                );
            }
        } else {
            println!("  no summary");
        }
    }
    let Ok(names) = std::env::var("CPPLS_C_NAMES") else {
        return;
    };
    println!("\n--- declarations: what the index holds under each name");
    let probe = std::env::var_os("CPPLS_C_PROBE_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("src").join("main.c"));
    for name in names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        match session.index().definitions(name, &probe) {
            Known::Yes(found) => {
                println!("  {name}: {} fact(s)", found.found.len());
                for definition in &found.found {
                    let fact = &definition.fact;
                    println!(
                        "    {:?}  {:?}  type {:?}  parameters {:?}  returns {:?}",
                        definition.file.file_name().unwrap_or_default(),
                        fact.kind,
                        fact.type_of.as_ref().map(|t| t.to_string()),
                        fact.parameter_list,
                        fact.returns.as_ref().map(|t| t.to_string())
                    );
                }
            }
            Known::No => println!("  {name}: no fact at all (the name query declined)"),
            Known::Unknown(reason) => println!("  {name}: unknown — {}", reason.describe()),
        }
    }

    // **A slice of one file's rendering**, for the questions a finding cannot answer on its own: `CPPLS_C_SHOW` names
    // the file and a needle, and the probe prints 400 bytes of the *rendering* around the first occurrence.
    if let Some(show) = std::env::var_os("CPPLS_C_SHOW")
        && let Some(show) = show.to_str()
        && let Some((file, needle)) = show.split_once('|')
    {
        // **The file's own text is searched too**, because the point of the probe is to answer "did the cook keep this",
        // and "the needle is not in the rendering" is an answer worth printing rather than a silent nothing.
        let held = std::fs::read_to_string(file).unwrap_or_default();
        println!(
            "  show: {file} held {} chars, needle in the file: {}, view: {}",
            held.len(),
            held.contains(needle),
            session.view(std::path::Path::new(file)).is_some()
        );
        let Some(view) = session.view(std::path::Path::new(file)) else {
            println!("  show: no view");
            return;
        };
        let Some(at) = view.source.find(needle) else {
            println!(
                "  show: the rendering has no {needle:?} ({} chars)",
                view.source.len()
            );
            return;
        };
        let from = at.saturating_sub(200).min(view.source.len());
        let to = (at + 1600).min(view.source.len());
        println!(
            "  rendering of {file} around {needle:?}:\n{}",
            &view.source[from..to]
        );
    }
}

fn name_of(path: &Path) -> String {
    path.strip_prefix(path.ancestors().nth(2).unwrap_or(path))
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// The labels an editor would show at `cursor` in `path` — the completion list, with the cursor's own position
/// resolved through the rendering.
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

/// What `type_at` says about the expression at `cursor` — the hover's answer, as a spelling.
fn declared_type(session: &Session<DiskFiles>, path: &Path, cursor: usize) -> Option<String> {
    let view = session.view(path)?;
    let reading = view.reading_offset_of(FileOffset::new(cursor))?;
    match session.type_at(&view, reading) {
        Known::Yes(found) => Some(format!("{} : {}", found.expression, found.type_of)),
        Known::Unknown(reason) => Some(format!("<unknown: {}>", reason.describe())),
        Known::No => None,
    }
}
