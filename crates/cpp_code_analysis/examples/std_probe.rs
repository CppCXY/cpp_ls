//! What a standard-library closure looks like to this parser: cost, cleanliness, and where it breaks.
//!
//! ```text
//! g++ -M -std=c++20 t.cpp | tr '\\' '/' | tr ' ' '\n' | sort -u > files.txt
//! cargo run --release -p cpp_code_analysis --example std_probe -- files.txt
//! ```
//!
//! `docs/std-library.md` records the numbers this prints and what they decide. It exists so that the numbers can
//! be reproduced after a fix rather than remembered: the point of the standard-library work is to move "failing
//! files" towards zero, and a claim about progress that cannot be re-measured is not a claim.
//!
//! Three things are printed, in increasing order of how much they decide:
//!
//! 1. **the cost** — files, lines, bytes, and how long the parse takes;
//! 2. **the census** — how many files parse cleanly, and which messages account for the rest, most common first.
//!    A message count is *not* a defect count: the standard headers cascade, so one unread construct costs a
//!    hundred errors;
//! 3. **the first error of every failing file**, with its source line — which is the only view that shows the
//!    *cause*, because the first error is the one nothing above it explains. Plus the share of those lines that
//!    mention a macro the closure defines, which is what says whether the dominant family is "a macro the parser
//!    does not know" or something else.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let key = cpp_code_analysis::SummaryKey::new(0, 0);

    // Every macro name the closure defines, which is the upper bound on what a table could know, and each
    // file's own — the difference between the two is the whole question of whether the file-local table is
    // enough.
    let mut closure_macros: HashSet<String> = HashSet::new();
    let mut own_macros: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    // The full summaries as well, because the positional second pass needs each file's **includes** and each
    // included file's macro facts — that is what `macros_from_direct_includes` turns into parser evidence.
    let mut summaries: HashMap<PathBuf, cpp_code_analysis::FileSummary> = HashMap::new();
    let mut definition_sources: HashMap<PathBuf, String> = HashMap::new();
    let seeded = std::env::args().any(|argument| argument == "--seeds");
    // `--closure` walks each direct include's **own** closure rather than stopping one hop down. The two answer
    // different questions — one hop is what the file literally includes, the closure is what the preprocessor
    // sees — and the difference is what the bill of remaining failures turns on (see `docs/index-design.md`).
    let closure = std::env::args().any(|argument| argument == "--closure");

    // **A real indexer, not the convenience `summarize`**: that one resolves no includes at all (`NoFiles`), so a
    // probe built on it seeds nothing and the whole positional experiment would be a silent no-op — which the
    // `positional macro evidence:` line below now reports, so the two cannot be confused again.
    //
    // The corpus *is* the search path: every directory a listed file lives in, offered for both forms of
    // `#include`. That is an approximation of what the compiler searched (no `-I` order, no `#include_next`
    // subtleties), and the seed count is what says how far it got.
    let files = cpp_code_analysis::DiskFiles;
    let mut config = cpp_code_analysis::CompilerConfig::default();
    let mut directories: HashSet<PathBuf> = HashSet::new();
    for path in &paths {
        if let Some(parent) = path.parent() {
            directories.insert(parent.to_path_buf());
        }
    }
    for directory in &directories {
        config = config
            .with_include_path(directory.clone())
            .with_system_include_path(directory.clone());
    }
    let indexer = cpp_code_analysis::FileIndexer::new(&files, &config);

    // **What the compilation starts with**: the `-D`s, the standard, and the ~480 names the compiler predefines.
    // Every `#if` in every header is a question about these, so without them the condition layer can only answer
    // `Unknown` — and an `Unknown` branch is a `#define` that is not evidence. Discovered once, from the same
    // compiler the corpus came from, and only when the seeds are asked for (it spawns the compiler).
    let macros_the_compilation_starts_with = (seeded && closure)
        .then(|| {
            paths.first().and_then(|first| {
                cpp_code_analysis::discover(
                    &files,
                    &cpp_code_analysis::DiskCommands,
                    None,
                    first,
                    &cpp_code_analysis::Environment::current(),
                )
            })
        })
        .flatten()
        .map(|toolchain| {
            cpp_code_analysis::graph::Marked::from_config(
                &toolchain.config(&cpp_code_analysis::CompilerConfig::default()),
            )
        });

    let started = std::time::Instant::now();
    for path in &paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let summary = indexer.index(path, &source, key);
        let names: HashSet<String> = summary
            .macros
            .iter()
            .filter(|fact| fact.kind.is_definition())
            .map(|fact| fact.name.clone())
            .collect();
        closure_macros.extend(names.iter().cloned());
        own_macros.insert(path.clone(), names);
        summaries.insert(path.clone(), summary);
        definition_sources.insert(path.clone(), source);
    }
    let indexed = started.elapsed();

    let mut clean = 0usize;
    let mut failing = 0usize;
    let mut by_message: HashMap<String, usize> = HashMap::new();
    let mut explained_by_own = 0usize;
    let mut explained_by_closure = 0usize;
    let mut details: Vec<String> = Vec::new();
    // How many errors each file has, so the histogram below can say how much of the tail is one construct away.
    let mut counts: Vec<usize> = Vec::new();
    // **How many seeds the run actually produced.** A census that is *meant* to change a reading and does not is
    // either a finding or a broken instrument, and the two are indistinguishable without this number: the
    // convenience `summarize` resolves no includes at all (`NoFiles`), so a probe built on it seeds **nothing**.
    // **How many decision points a parse has**: how often the grammar asks what a name is as a macro, and about how
    // many distinct names. This is the number `docs/index-design.md` gates expansion on — the work expansion adds
    // is `Σ(decision points) × O(body)`, and it has to be small enough to be invisible next to the parse itself.
    let mut macro_questions = 0usize;
    let mut macro_question_names = 0usize;
    let mut busiest_questions = 0usize;
    let mut total_seeds = 0usize;
    let mut bodies_with_text = 0usize;
    let mut files_with_seeds = 0usize;
    let mut seeding_time = std::time::Duration::ZERO;
    // How many conditional facts the closure walk met, and how many of them the condition layer put **in force**.
    // Two numbers rather than one, because "no conditional evidence" and "conditional evidence that was asked
    // about and refused" are different worlds and the corpus numbers look the same in both.
    let conditional_asked = std::cell::Cell::new(0usize);
    let conditional_taken = std::cell::Cell::new(0usize);
    // Replacement lists of macros whose definition is conditional but in force — the second channel, the one a rule
    // reads (`MacroEnvironment::with_bodies_in_force`). Counted apart from the definitions on purpose.
    let mut bodies_in_force = 0usize;
    // The **shape** distribution of what the seeds say. A name alone enables the rules that ask "is this a macro",
    // and a shape is what enables the ones that ask "what may stand here" — so "the evidence arrived and changed
    // nothing" has two very different causes, and this is what tells them apart.
    let mut seeds_by_shape: HashMap<&'static str, usize> = HashMap::new();
    let mut total_bytes = 0usize;
    let mut total_lines = 0usize;

    let started = std::time::Instant::now();
    for path in &paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        total_bytes += source.len();
        total_lines += source.lines().count();

        // **The positional second pass**: what the file's own `#include`s contribute, each macro in force from the
        // offset its include ended at. `--seeds` turns it on, so the two censuses are the same command otherwise.
        let Some(summary) = summaries.get(path) else {
            continue;
        };
        let environment = seeded.then(|| {
            // Building the evidence is itself a measurement: the closure version reads the whole include graph of
            // every direct include, so its cost is the thing that decides whether this layer can be per-file.
            let started = std::time::Instant::now();
            let (seeds, bodies) = if closure {
                let table = macros_the_compilation_starts_with.as_ref();
                let evidence = cpp_code_analysis::macros_from_the_closure_with_bodies(
                    summary,
                    |wanted| {
                        summaries.get(wanted).map(|summary| {
                            (summary, definition_sources.get(wanted).map(String::as_str).unwrap_or(""))
                        })
                    },
                    // **The condition layer, in one line.** A `#define` inside an `#if` is read only when that
                    // branch was taken, and the branch is decided against the macros the compilation starts with —
                    // `-D`s, `-std=`, and the ~480 names the compiler predefines. Without a toolchain every
                    // condition is `Unknown`, which is why the two counts below are printed at all: "no conditional
                    // evidence" and "conditional evidence asked about and refused" look alike in the corpus numbers.
                    //
                    // What comes back is **two channels**: definitions that are in force (which say *whether a name
                    // is a macro*) and bodies that are in force (which a rule *reads*). Handing the second lot out as
                    // definitions is what the first version did, and it cost 3 files — see `ClosureEvidence`.
                    |file, fact| match table {
                        Some(table) => {
                            conditional_asked.set(conditional_asked.get() + 1);
                            let in_force =
                                cpp_code_analysis::index::environment::fact_in_force(table, file, fact);
                            if in_force {
                                conditional_taken.set(conditional_taken.get() + 1);
                            }
                            in_force
                        }
                        None => false,
                    },
                );
                (evidence.macros, evidence.conditional_bodies)
            } else {
                (
                    cpp_code_analysis::macros_from_direct_includes_with_bodies(summary, |wanted| {
                        summaries.get(wanted).map(|summary| {
                            (summary, definition_sources.get(wanted).map(String::as_str).unwrap_or(""))
                        })
                    }),
                    Vec::new(),
                )
            };
            seeding_time += started.elapsed();
            bodies_in_force += bodies.len();
            total_seeds += seeds.len();
            bodies_with_text += seeds
                .iter()
                .filter(|seed| seed.body_text.as_deref().is_some_and(|text| !text.trim().is_empty()))
                .count();
            if !seeds.is_empty() {
                files_with_seeds += 1;
            }
            for seed in &seeds {
                if let Some(cpp_parser::SymbolKind::Macro { body, .. }) = &seed.definition {
                    let shape = match body {
                        cpp_parser::MacroBody::Specifier => "Specifier",
                        cpp_parser::MacroBody::Statement => "Statement",
                        cpp_parser::MacroBody::Block => "Block",
                        cpp_parser::MacroBody::Expression => "Expression",
                        cpp_parser::MacroBody::Type => "Type",
                        cpp_parser::MacroBody::Unknown => "Unknown",
                    };
                    *seeds_by_shape.entry(shape).or_default() += 1;
                }
            }
            cpp_parser::MacroEnvironment::from_included_macros(seeds).with_bodies_in_force(bodies)
        });

        let config = match &environment {
            Some(environment) => cpp_parser::ParserConfig::default().with_macros_from_includes(environment),
            None => cpp_parser::ParserConfig::default(),
        };
        let (tree, audit) = cpp_parser::CppParser::parse_with_audit(&source, config);
        macro_questions += audit.macro_questions;
        if audit.macro_question_names > macro_question_names {
            macro_question_names = audit.macro_question_names;
        }
        if audit.macro_questions > busiest_questions {
            busiest_questions = audit.macro_questions;
        }
        let errors = tree.get_errors();

        if errors.is_empty() {
            clean += 1;
            counts.push(0);
            // The invariants hold on this corpus too, and are checked rather than assumed: a header is not a
            // gentler input than a test fixture.
            assert_eq!(
                tree.to_source_text(),
                source,
                "losslessness broke on {}",
                path.display()
            );
            continue;
        }

        failing += 1;
        counts.push(errors.len());
        for error in errors {
            *by_message.entry(error.message.clone()).or_default() += 1;
        }

        let index = cpp_parser::LineIndex::parse(&source);
        let Some((line, column)) = index.get_line_col(errors[0].range.start(), &source) else {
            continue;
        };

        // The offending line and the two before it: a macro that breaks a declaration is usually written on the
        // line itself or the one above, and a diagnostic often lands a line late.
        let window: Vec<&str> = source
            .lines()
            .skip(line.saturating_sub(2))
            .take(3)
            .collect();
        let mentions = |names: &HashSet<String>| {
            window.join(" ").split(|c: char| !(c.is_alphanumeric() || c == '_')).any(
                |word| word.len() > 2 && names.contains(word),
            )
        };

        if own_macros.get(path).is_some_and(&mentions) {
            explained_by_own += 1;
        }
        if mentions(&closure_macros) {
            explained_by_closure += 1;
        }

        details.push(format!(
            "{:>4}:{:<3} {:<44} | {} :: {}",
            line + 1,
            column,
            errors[0].message,
            window.last().unwrap_or(&"").trim().chars().take(70).collect::<String>(),
            path.file_name().unwrap_or_default().to_string_lossy(),
        ));
    }
    let parsed = started.elapsed();

    // Which experiment this run is, stated in the output: "the evidence arrived and changed nothing" and "the
    // evidence was never built" look identical in the numbers, and that confusion has already cost this project
    // one vacuous census (see `docs/roadmap.md`, B82).
    if seeded {
        println!(
            "seeding mode: {}\n",
            if closure {
                "the closure of each direct include"
            } else {
                "direct includes only"
            }
        );
    }

    let conditional_asked = conditional_asked.get();
    let conditional_taken = conditional_taken.get();

    println!(
        "files {} | clean {} | failing {} | {} KB | {} lines\n\
         index (parse + scopes + facts) {:?} | parse alone {:?}\n\
         {failing} failures: first error on a line mentioning a macro this file defines {explained_by_own} \
         ({:.0}%), any macro the closure defines {explained_by_closure} ({:.0}%)\n\
         positional macro evidence: {total_seeds} seeds over {files_with_seeds} files ({bodies_with_text} with body text), \
built in {seeding_time:?}\n\
         conditional facts met {conditional_asked} | branches in force {conditional_taken} | bodies in force \
{bodies_in_force} (no toolchain means none can be answered)\n\
         seed shapes: {}\n\
         decision points: {macro_questions} macro questions | {macro_question_names} name-questions, summed \
over the files | busiest file {busiest_questions}",
        paths.len(),
        clean,
        failing,
        total_bytes / 1024,
        total_lines,
        indexed,
        parsed,
        explained_by_own as f64 * 100.0 / failing.max(1) as f64,
        explained_by_closure as f64 * 100.0 / failing.max(1) as f64,
        {
            let mut pairs: Vec<(&str, usize)> = seeds_by_shape.iter().map(|(k, v)| (*k, *v)).collect();
            pairs.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            pairs
                .into_iter()
                .map(|(shape, count)| format!("{shape} {count}"))
                .collect::<Vec<_>>()
                .join(" | ")
        },
    );

    let mut ranked: Vec<(&String, &usize)> = by_message.iter().collect();
    ranked.sort_by(|one, other| other.1.cmp(one.1));
    // **The total is printed, not just the top of the list**: the list is truncated, so "add up what you see" is
    // not the number — and every number in `docs/` that came from this probe is a total. (Found the hard way:
    // 15 lines summed to 920 while the file really had 941 messages, because the tail beyond the top 15 is real.)
    let messages: usize = by_message.values().sum();
    println!(
        "\n--- messages: {messages} in total over {} kinds, the 15 most common first \
         (a count is not a defect count: these cascade) ---",
        by_message.len()
    );
    for (message, count) in ranked.iter().take(15) {
        println!("{count:6}  {message}");
    }

    // How much of the tail is "one construct away"? A file with a single error is a file one rule from clean,
    // while a file with fifty is a file whose first error hid everything after it — and the two want different
    // work, which a list of first errors cannot say.
    let mut histogram = [0usize; 4];
    for count in counts.iter() {
        histogram[match count {
            0 => 0,
            1 => 1,
            2..=5 => 2,
            _ => 3,
        }] += 1;
    }
    println!(
        "\n--- how many errors each file has ---\n\
         clean {} | exactly one {} | two to five {} | more {}",
        histogram[0], histogram[1], histogram[2], histogram[3]
    );

    println!("\n--- the first error of every failing file, which is the one nothing above explains ---");
    for detail in &details {
        println!("{detail}");
    }
}
