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
    // `--macro NAME` asks what the environment knows about **one name** in each file, channel by channel. It is the
    // question that separates "the evidence was never built" from "the evidence is there and no rule used it" —
    // the same distinction the seed counts make one level up, asked about the macro a rule is waiting for.
    // `--macro NAME[,NAME…]` asks what the environment knows about those names in each file, channel by channel. It
    // is the question that separates "the evidence was never built" from "the evidence is there and no rule used
    // it" — the same distinction the seed counts make one level up, asked about the macros a rule is waiting for.
    // A list, because a condition is answered from a *chain* of names: asking about one of them at a time is how a
    // chain takes five runs to trace.
    let watched: Vec<String> = std::env::args()
        .position(|argument| argument == "--macro")
        .and_then(|at| std::env::args().nth(at + 1))
        .map(|names| names.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    // `--standard c++17`: what the **configuration** decides, injected as predefined macros. It is a flag rather
    // than a constant because the point is to measure it: a corpus read as C++11 and the same corpus read as C++20
    // answer `#if __cplusplus >= …` differently, and which branches that wakes up is a fact about the corpus.
    let standard: Option<String> = std::env::args()
        .position(|argument| argument == "--standard")
        .and_then(|at| std::env::args().nth(at + 1));
    // `--no-toolchain` is the "no compiler was found" world: then the **configuration alone** is what the
    // condition layer has, which is exactly what injecting the standard and the target is for.
    let without_toolchain = std::env::args().any(|argument| argument == "--no-toolchain");

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
    if let Some(standard) = standard.as_deref() {
        config = config.with_standard(standard);
    }
    let indexer = cpp_code_analysis::FileIndexer::new(&files, &config);

    // **What the compilation starts with** — and it is built even when no compiler was found, because that is the
    // case the configuration is supposed to cover by itself: `-std=c++17` decides `__cplusplus`, a target triple
    // decides the platform names, and a compiler that answered `-dM` is the *fuller* answer rather than the only
    // one. A run with neither is `incomplete()` — "nobody said" — which the evaluator turns into `Unknown`, so no
    // branch is answered wrongly; see `predefined_macros_of` and `Session`'s environment, which is built the same
    // way.
    let seed = if seeded && closure {
        let mut marked = cpp_code_analysis::graph::Marked::default();
        let mut found_a_toolchain = false;

        if !without_toolchain
            && let Some(toolchain) = paths.first().and_then(|first| {
                cpp_code_analysis::discover(
                    &files,
                    &cpp_code_analysis::DiskCommands,
                    None,
                    first,
                    &cpp_code_analysis::Environment::current(),
                    &cpp_code_analysis::include::msvc::WindowsLayout::current(),
                )
            })
        {
            found_a_toolchain = true;
            for (name, value) in toolchain.macros() {
                marked.define_on_the_command_line(name, value);
            }
        }

        // …and then what the **configuration** decides, over the top: the project's `-std=` beats the compiler's
        // own default invocation, which is the point `Toolchain::search_paths` makes about passing it on.
        for definition in cpp_code_analysis::predefined_macros_of(&config) {
            marked.define_on_the_command_line(&definition.name, definition.value.as_deref());
        }

        if found_a_toolchain { marked } else { marked.incomplete() }
    } else {
        // Not seeding at all: an empty, incomplete state, so that nothing here pretends to know anything.
        cpp_code_analysis::graph::Marked::default().incomplete()
    };

    // The seed is what every `#ifdef __cplusplus` in the corpus is answered against, so whether it really holds the
    // compiler's names is worth one line rather than an assumption: `Unknown` silently keeps every conditional
    // `#define` out of the evidence, and a wrong branch answers the wrong reading. Printed whether or not a
    // toolchain was found, because "no compiler" is a case the configuration is supposed to cover by itself — the
    // **value** `__cplusplus` holds is asserted by `predefined_macros_of`'s own tests.
    if seeded && closure {
        use cpp_code_analysis::condition::MacroValues;

        println!(
            "compilation seed: __cplusplus defined {:?} | _WIN32 defined {:?} | uncertain about __cplusplus {} | \
standard {}",
            seed.lookup("__cplusplus").is_defined(),
            seed.lookup("_WIN32").is_defined(),
            seed.is_uncertain("__cplusplus"),
            standard.as_deref().unwrap_or("(none — the configuration decides nothing)"),
        );
    }

    // One cache of parsed #defines for the whole run: a definition does not depend on which file is being seeded, so the feed costs one parse per definition rather than one per definition per file (B95).
    let mut macro_definitions = cpp_code_analysis::MacroDefinitions::default();

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

    // **Who includes each file**, and where — the translation unit's half of the environment. Read off the include
    // facts of the indexed files, first includer wins: a header may be reached from several places, and any one of
    // them is a real translation unit it belongs to (the compile database would name the intended one).
    let mut includers: HashMap<PathBuf, (PathBuf, usize)> = HashMap::new();
    for (path, summary) in &summaries {
        for include in &summary.includes {
            let Some(resolved) = include.resolved.as_ref() else {
                continue;
            };
            if summaries.contains_key(resolved) {
                includers
                    .entry(resolved.clone())
                    .or_insert_with(|| (path.clone(), include.range.start_offset));
            }
        }
    }

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
    let mut conditional_asked = 0usize;
    let mut conditional_taken = 0usize;
    // Replacement lists of macros whose definition is conditional but in force — the second channel, the one a rule
    // reads (`MacroEnvironment::with_bodies_in_force`). Counted apart from the definitions on purpose.
    let mut bodies_in_force = 0usize;
    // How many files were read **inside** a translation unit that includes them — the includer's macros are part
    // of what a header sees, and this says whether the corpus could supply them at all.
    let mut files_with_context = 0usize;
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
        // What the **includer** contributed, for the `--macro` line below: whether the translation unit's half of the
        // environment was built at all, and whether it carries the name being watched.
        let mut context_seeds = 0usize;
        let mut context_carries_the_watched_name = false;
        let environment = seeded.then(|| {
            // Building the evidence is itself a measurement: the closure version reads the whole include graph of
            // every direct include, so its cost is the thing that decides whether this layer can be per-file.
            let started = std::time::Instant::now();
            let (seeds, bodies) = if closure {
                let look_up = |wanted: &std::path::Path| {
                    summaries.get(wanted).map(|summary| {
                        (summary, definition_sources.get(wanted).map(String::as_str).unwrap_or(""))
                    })
                };
                let evidence =
                    cpp_code_analysis::macros_from_the_closure_with_bodies(summary, look_up, &seed, &mut macro_definitions);
                conditional_asked += evidence.conditional_facts;
                conditional_taken += evidence.facts_in_force;

                // **This file as part of the translation unit that includes it**: the includer's own macros up to
                // the point of its `#include`, seeded at offset 0 because another file's offsets mean nothing here.
                // That is the only way a header sees a name none of its own includes define — `commdlg.h` and
                // `STDMETHOD`, whose definition is in a file its own include list does not mention
                // (`docs/grammar-gaps.md` B90/B91).
                let context = includers.get(path).map(|(includer, at)| {
                    let summary = summaries.get(includer).expect("an includer is indexed");
                    cpp_code_analysis::macros_in_force_before_the_include(summary, *at, look_up, &seed, &mut macro_definitions)
                });
                if context.is_some() {
                    files_with_context += 1;
                }
                if let Some(context) = context.as_ref() {
                    context_seeds = context.macros.len() + context.conditional_bodies.len();
                    context_carries_the_watched_name = watched.iter().any(|name| {
                        context.macros.iter().any(|entry| &*entry.name == name)
                            || context
                                .conditional_bodies
                                .iter()
                                .any(|(defined, _)| &**defined == name)
                    });
                    conditional_asked += context.conditional_facts;
                    conditional_taken += context.facts_in_force;
                }

                // The file's **own** closure comes second, so a name its own includes define wins over the unit's.
                let (mut seeds, mut bodies) = match context {
                    Some(context) => (context.macros, context.conditional_bodies),
                    None => (Vec::new(), Vec::new()),
                };
                seeds.extend(evidence.macros);
                bodies.extend(evidence.conditional_bodies);
                (seeds, bodies)
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

        if let Some(environment) = environment.as_ref() {
            for name in &watched {
                let Some(at) = source.find(name.as_str()) else {
                    continue;
                };

                let trim = |text: Option<&str>| {
                    text.map(|text| text.trim().chars().take(48).collect::<String>())
                };
                println!(
                    "MACRO {name} in {:<24} evidence {:<5} | positional body {:?} | in-force body {:?} | context \
{context_seeds} seeds, carries it {context_carries_the_watched_name}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    environment.kind_of(name, at).is_some(),
                    trim(environment.body_text_of(name, at)),
                    trim(environment.body_text_in_force(name)),
                );
            }
        }
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

        // **Which of the names on the line the environment can actually *read*** — the question that separates
        // "the evidence was never built" from "the evidence is there and no rule used it", which look identical in
        // every other number this probe prints. A name is listed when a **body** for it is in force: the file's own
        // `#define` or one the closure carried in.
        let line_text = window.join(" ");
        let readable: Vec<&str> = line_text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|word| word.len() > 2)
            .filter(|word| {
                environment.as_ref().is_some_and(|environment| {
                    // **Both channels**, because a body can arrive either way: positionally (an unconditional
                    // `#define`) or through the conditional one that only a branch in force fills.
                    environment.body_text_of(word, 0).is_some()
                        || environment.body_text_in_force(word).is_some()
                })
            })
            .collect();
        let readable = if readable.is_empty() {
            String::new()
        } else {
            format!(" | bodies in force: {}", readable.join(", "))
        };

        details.push(format!(
            "{:>4}:{:<3} {:<44} | {} :: {}{}",
            line + 1,
            column,
            errors[0].message,
            window.last().unwrap_or(&"").trim().chars().take(70).collect::<String>(),
            path.file_name().unwrap_or_default().to_string_lossy(),
            readable,
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


    println!(
        "files {} | clean {} | failing {} | {} KB | {} lines\n\
         index (parse + scopes + facts) {:?} | parse alone {:?}\n\
         {failing} failures: first error on a line mentioning a macro this file defines {explained_by_own} \
         ({:.0}%), any macro the closure defines {explained_by_closure} ({:.0}%)\n\
         positional macro evidence: {total_seeds} seeds over {files_with_seeds} files ({bodies_with_text} with body text), \
built in {seeding_time:?}\n\
         conditional facts met {conditional_asked} | branches in force {conditional_taken} | bodies in force \
{bodies_in_force} (no toolchain means none can be answered)\n\
         read inside an includer {files_with_context} files (the translation unit's half of the environment)\n\
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

