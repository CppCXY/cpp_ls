//! **A measurement of the C corpus** — one line per file, one line per member question, one line per type question.
//!
//! ```text
//! $env:CPPLS_C_CORPUS = "C:\Users\zc\Desktop\cpp"     # the default when it exists
//! cargo test --release -p cpp_code_analysis --test c_corpus -- --ignored --nocapture
//! ```
//!
//! The corpus is a real project: `CMakeLists.txt` builds it with MinGW's gcc, `build/compile_commands.json` carries the
//! flags, and every construct in it is one the compiler accepted. That is what makes the numbers here mean something —
//! a file that compiles and that this analysis reports an error in is a defect of the analysis, not of the file.
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
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\cpp"));
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

/// The member questions. Grouped by the C feature that makes each one interesting, which is why the list is
/// hand-written rather than harvested: every row is a construct this corpus exists to measure.
const MEMBERS: &[MemberRow] = &[
    // The standard library's own structs — `struct tm`, `div_t`, `struct lconv` — whose definitions live in the
    // toolchain's include directory rather than in the project.
    MemberRow {
        file: "src/library.c",
        needle: "local.tm_year",
        expected: "tm_year",
    },
    MemberRow {
        file: "src/library.c",
        needle: "local.tm_wday",
        expected: "tm_wday",
    },
    MemberRow {
        file: "src/library.c",
        needle: "local.tm_yday",
        expected: "tm_yday",
    },
    MemberRow {
        file: "src/library.c",
        needle: "local.tm_isdst",
        expected: "tm_isdst",
    },
    MemberRow {
        file: "src/library.c",
        needle: "pointer->tm_year",
        expected: "tm_year",
    },
    MemberRow {
        file: "src/library.c",
        needle: "from_library->tm_mday",
        expected: "tm_mday",
    },
    MemberRow {
        file: "src/library.c",
        needle: "divided.quot",
        expected: "quot",
    },
    MemberRow {
        file: "src/library.c",
        needle: "divided.rem",
        expected: "rem",
    },
    MemberRow {
        file: "src/library.c",
        needle: "long_divided.quot",
        expected: "quot",
    },
    MemberRow {
        file: "src/library.c",
        needle: "wide_divided.rem",
        expected: "rem",
    },
    MemberRow {
        file: "src/library.c",
        needle: "locale->decimal_point",
        expected: "decimal_point",
    },
    MemberRow {
        file: "src/library.c",
        needle: "locale->int_frac_digits",
        expected: "int_frac_digits",
    },
    MemberRow {
        file: "src/library.c",
        needle: "locale->p_cs_precedes",
        expected: "p_cs_precedes",
    },
    MemberRow {
        file: "src/library.c",
        needle: "locale->negative_sign",
        expected: "negative_sign",
    },
    // Our own aggregates: a struct through a pointer, a nested struct, an anonymous union, a bitfield.
    MemberRow {
        file: "src/aggregates.c",
        needle: "pointer->x",
        expected: "x",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "sum.x",
        expected: "x",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "buffer->length",
        expected: "length",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "buffer->data",
        expected: "data",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "head->next",
        expected: "next",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "cursor->name",
        expected: "name",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "A_LINE.integer",
        expected: "integer",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "A_LINE.colour",
        expected: "colour",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "A_LINE.corners",
        expected: "corners",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "pointer->from",
        expected: "from",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "line.colour",
        expected: "colour",
    },
    MemberRow {
        file: "src/aggregates.c",
        needle: "a_value.as",
        expected: "as",
    },
    // types.c: a union inside a struct, an anonymous union, every bitfield base type.
    MemberRow {
        file: "src/types.c",
        needle: "value.as",
        expected: "as",
    },
    MemberRow {
        file: "src/types.c",
        needle: "line.integer",
        expected: "integer",
    },
    MemberRow {
        file: "src/types.c",
        needle: "line.colour",
        expected: "colour",
    },
    MemberRow {
        file: "src/types.c",
        needle: "fields.single_bit",
        expected: "single_bit",
    },
    MemberRow {
        file: "src/types.c",
        needle: "fields.signed_field",
        expected: "signed_field",
    },
    MemberRow {
        file: "src/types.c",
        needle: "fields.wide",
        expected: "wide",
    },
    MemberRow {
        file: "src/types.c",
        needle: "fields.permissions",
        expected: "permissions",
    },
    MemberRow {
        file: "src/types.c",
        needle: "flags.generation",
        expected: "generation",
    },
    MemberRow {
        file: "src/types.c",
        needle: "flags.ready",
        expected: "ready",
    },
    MemberRow {
        file: "src/types.c",
        needle: "anonymous_union.integer",
        expected: "integer",
    },
    // control.c and preprocessor.c: a member behind a macro-expanded name, and one on the right of `&&`.
    MemberRow {
        file: "src/control.c",
        needle: "flags.ready",
        expected: "ready",
    },
    MemberRow {
        file: "src/control.c",
        needle: "flags.generation",
        expected: "generation",
    },
    MemberRow {
        file: "src/control.c",
        needle: "origin.x",
        expected: "x",
    },
    MemberRow {
        file: "src/preprocessor.c",
        needle: "pointer->x",
        expected: "x",
    },
    MemberRow {
        file: "src/preprocessor.c",
        needle: "origin.x",
        expected: "x",
    },
    // gnu.c: a member of a `packed` struct reached through a cast, and one through `typeof`.
    MemberRow {
        file: "src/gnu.c",
        needle: "other_pointer->x",
        expected: "x",
    },
    MemberRow {
        file: "src/gnu.c",
        needle: "gnu_aligned_buffer)->value",
        expected: "value",
    },
    // c23.c: a `constexpr` aggregate, a `_BitInt` bitfield, an `auto`-deduced struct.
    MemberRow {
        file: "src/c23.c",
        needle: "C23_ORIGIN.x",
        expected: "x",
    },
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
    MemberRow {
        file: "src/c23.c",
        needle: "point_copy.x",
        expected: "x",
    },
    MemberRow {
        file: "src/c23.c",
        needle: "other->x",
        expected: "x",
    },
    MemberRow {
        file: "src/c23.c",
        needle: "origin.x",
        expected: "x",
    },
    // The header's own `static inline` functions: a member access whose declaration is in a *header*, asked from a
    // file that only includes it.
    MemberRow {
        file: "include/corpus.h",
        needle: "a->x",
        expected: "x",
    },
    MemberRow {
        file: "include/corpus.h",
        needle: "flags->ready",
        expected: "ready",
    },
    MemberRow {
        file: "include/corpus.h",
        needle: "line->integer",
        expected: "integer",
    },
];

/// The type questions: the declared name, and what the file says its type is.
const TYPES: &[TypeRow] = &[
    // types.c — every arithmetic type, in a member and in a local.
    TypeRow {
        file: "src/types.c",
        needle: "struct arithmetic values",
        name: "values",
        expected: "arithmetic",
    },
    TypeRow {
        file: "src/types.c",
        needle: "int promoted",
        name: "promoted",
        expected: "int",
    },
    TypeRow {
        file: "src/types.c",
        needle: "unsigned int converted",
        name: "converted",
        expected: "unsigned int",
    },
    TypeRow {
        file: "src/types.c",
        needle: "long widened",
        name: "widened",
        expected: "long",
    },
    TypeRow {
        file: "src/types.c",
        needle: "double mixed",
        name: "mixed",
        expected: "double",
    },
    TypeRow {
        file: "src/types.c",
        needle: "const int *pointer_to_const =",
        name: "pointer_to_const",
        expected: "int",
    },
    TypeRow {
        file: "src/types.c",
        needle: "enum constants constant_value",
        name: "constant_value",
        expected: "constants",
    },
    // aggregates.c — a struct, a pointer to one, a flexible array member's owner, an array, a VLA.
    TypeRow {
        file: "src/aggregates.c",
        needle: "point sum =",
        name: "sum",
        expected: "point",
    },
    TypeRow {
        file: "src/aggregates.c",
        needle: "point *pointer = &sum;",
        name: "pointer",
        expected: "point",
    },
    TypeRow {
        file: "src/aggregates.c",
        needle: "struct buffer *buffer",
        name: "buffer",
        expected: "buffer",
    },
    TypeRow {
        file: "src/aggregates.c",
        needle: "char text[]",
        name: "text",
        expected: "char",
    },
    TypeRow {
        file: "src/aggregates.c",
        needle: "int vla[count]",
        name: "vla",
        expected: "int",
    },
    TypeRow {
        file: "src/aggregates.c",
        needle: "node second =",
        name: "second",
        expected: "node",
    },
    // functions.c — a function pointer, and a variadic function's parameter.
    TypeRow {
        file: "src/functions.c",
        needle: "binary_op operation = add",
        name: "operation",
        expected: "binary_op",
    },
    TypeRow {
        file: "src/functions.c",
        needle: "comparer *compare",
        name: "compare",
        expected: "comparer",
    },
    TypeRow {
        file: "src/functions.c",
        needle: "node *head = malloc",
        name: "head",
        expected: "node",
    },
    // library.c — the library's own types.
    TypeRow {
        file: "src/library.c",
        needle: "struct tm local;",
        name: "local",
        expected: "tm",
    },
    TypeRow {
        file: "src/library.c",
        needle: "struct tm *pointer = &local;",
        name: "pointer",
        expected: "tm",
    },
    TypeRow {
        file: "src/library.c",
        needle: "div_t divided",
        name: "divided",
        expected: "div_t",
    },
    TypeRow {
        file: "src/library.c",
        needle: "lldiv_t wide_divided",
        name: "wide_divided",
        expected: "lldiv_t",
    },
    TypeRow {
        file: "src/library.c",
        needle: "FILE *file =",
        name: "file",
        expected: "FILE",
    },
    TypeRow {
        file: "src/library.c",
        needle: "struct lconv *locale",
        name: "locale",
        expected: "lconv",
    },
    TypeRow {
        file: "src/library.c",
        needle: "jmp_buf environment",
        name: "environment",
        expected: "jmp_buf",
    },
    TypeRow {
        file: "src/library.c",
        needle: "time_t now =",
        name: "now",
        expected: "time_t",
    },
    TypeRow {
        file: "src/library.c",
        needle: "size_t length = strlen",
        name: "length",
        expected: "size_t",
    },
    // gnu.c — the extension types.
    TypeRow {
        file: "src/gnu.c",
        needle: "__int128 wide =",
        name: "wide",
        expected: "__int128",
    },
    TypeRow {
        file: "src/gnu.c",
        needle: "__auto_type value = 7;",
        name: "value",
        expected: "int",
    },
    TypeRow {
        file: "src/gnu.c",
        needle: "__auto_type pointer = &point_copy;",
        name: "pointer",
        expected: "point",
    },
    TypeRow {
        file: "src/gnu.c",
        needle: "struct gnu_packed *)",
        name: "gnu_packed",
        expected: "gnu_packed",
    },
    // c23.c — `auto`, `constexpr`, `typeof`.
    TypeRow {
        file: "src/c23.c",
        needle: "auto count = 3;",
        name: "count",
        expected: "int",
    },
    TypeRow {
        file: "src/c23.c",
        needle: "auto scale = 2.5;",
        name: "scale",
        expected: "double",
    },
    TypeRow {
        file: "src/c23.c",
        needle: "auto pointer = &point_copy;",
        name: "pointer",
        expected: "point",
    },
    TypeRow {
        file: "src/c23.c",
        needle: "typeof(value) copy",
        name: "copy",
        expected: "int",
    },
];

/// The corpus's own files, sorted, without the build directory.
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
            // **The rendering, because that is the text a finding's offset is into.** A file whose rendering inlines its
            // include closure has the whole C library before its own first line, so a finding's *file* line cannot be
            // computed from its offset — but the rendering line and the text at the offset say whether the finding is
            // about this file's code or about a header's.
            let rendering = session
                .view(path)
                .map(|view| view.source.to_string())
                .unwrap_or_default();
            // **A window around the offset in the file**, because every range the diagnostics channel reports has been
            // mapped into the file — `add_in_the_file` for a rule that reads the summary, `map_into_the_file` for one
            // that reads the rendering — so the file is the text an offset is into, and the rendering is only there to
            // say what the cook made of the file.
            let where_at = |offset: usize| -> String {
                let text = &text;
                let from = offset.saturating_sub(64).min(text.len());
                let to = (offset + 64).min(text.len());
                let before = text[..from]
                    .char_indices()
                    .rev()
                    .next()
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
                    "      {}  ← {}",
                    where_at(error.start),
                    error.message.chars().take(90).collect::<String>()
                );
            }
            // **Every finding, grouped by code** — the list is the work list, and three samples per file is how the
            // first version of this probe hid that one code accounted for all of them. `CPPLS_C_ALL` lifts the cap.
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
                        "        {}  ← {}",
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

        let labels = member_labels(&session, &path, cursor);
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
/// `CPPLS_C_NAMES="functions_selector,functions_aborts,div,ldiv"` prints every fact filed under each name, with the
/// file it came from, its kind, its type and its parameter list. A name with no fact at all is a declaration the
/// summary did not record; a fact with an empty type is a declaration it recorded but could not read.
fn dump_names(session: &Session<DiskFiles>, root: &Path) {
    let Ok(names) = std::env::var("CPPLS_C_NAMES") else {
        return;
    };
    println!("\n--- declarations: what the index holds under each name");
    // **Where the toolchain said the headers are**: for a C project they must be the C ones (cstdlib is C++'s, and
    // a <stdlib.h> that resolves to it leaves div undeclared — measured).
    println!(
        "  named compilers: {:?}",
        session.discovery().named_compilers()
    );
    if let Some(toolchain) = session.toolchain() {
        println!(
            "  toolchain compiler: {:?}  version: {:?}",
            toolchain.compiler, toolchain.version
        );
        let paths: Vec<String> = toolchain
            .include_paths()
            .iter()
            .map(|path| format!("{:?}", path))
            .collect();
        println!("  toolchain include paths: {paths:?}");
    }
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
        && let Some(view) = session.view(std::path::Path::new(file))
        && let Some(at) = view.source.find(needle)
    {
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
