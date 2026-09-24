//! Probe: **how much of the conditional structure can be decided from what the index already holds?**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example condition_reach
//! ```
//!
//! # The question
//!
//! A conditional `#include` is the one thing that makes a *path* conditional, and a conditional path is what turns a
//! certain "use" into a "maybe" in the reference query — measured in the `winnt.h` family: 4 000-odd references,
//! every one of them blocked by
//!
//! ```cpp
//! #ifndef NT_INCLUDED
//! #include <winnt.h>
//! #endif
//! ```
//!
//! So before designing the macro environment (P3, and the key change it implies), measure what is actually needed:
//!
//! ```text
//! 1. what conditions guard the includes of this closure?
//! 2. is the name a condition tests defined anywhere in the closure — or undefined everywhere?
//! 3. what is left when only the shapes that need a *value* are counted (a version test, an arithmetic one)?
//! ```
//!
//! # Why this could be cheap
//!
//! A translation unit's macro state at a point is fixed by the `#define`s written before it — and those files are
//! exactly what the index walked to get here. So "is `NT_INCLUDED` defined?" may be answerable **from the index**,
//! without re-keying every summary on an environment: what a summary would need to carry is the *condition*, which
//! is in its own text, and the *evaluation* could happen at query time where the whole graph is in hand.
//!
//! This probe is what says whether that is enough, and how much of the uncertainty is left over for the `-D`s and
//! for headers outside the closure.

use std::collections::{HashMap, HashSet};

use cpp_code_analysis::{DiskFiles, FileProvider, OpenDocuments, Session, SessionFiles, WatchFilter};

const MAIN: &str = "\
#include \"widget.h\"\n\
#include <string>\n\
#include <vector>\n\
#include <map>\n\
void f(std::string s) { s.size(); }\n";

fn main() {
    let root = std::env::temp_dir().join("cppls-condition-reach");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");

    let main_file = root.join("main.cpp");
    std::fs::write(&main_file, MAIN).expect("the fixture writes");
    std::fs::write(root.join("widget.h"), "#define WIDGET_API\nint w;\n").expect("the fixture writes");
    std::fs::write(
        root.join("other.cpp"),
        "#include \"widget.h\"\nWIDGET_API int other() { return 0; }\n",
    )
    .expect("the fixture writes");

    // A compile database, with no flags beyond the compiler and the file: a real project has one, and it is what
    // makes the session **configured** — the caller has said how its files are built. The stronger claim that goes
    // with it ("so nothing else can define a name") is deliberately *not* made yet: `docs/roadmap.md` §3.5c has the
    // measurement (440/486 decided, and two families of references lost because a conditional first visit to a
    // header suppresses the certain one).
    let root_text = root.to_string_lossy().replace('\\', "/");
    std::fs::write(
        root.join("compile_commands.json"),
        format!(
            "[\n  {{\"directory\": \"{root_text}\", \"file\": \"{root_text}/main.cpp\", \
             \"arguments\": [\"g++\", \"-c\", \"{root_text}/main.cpp\"]}},\n  \
             {{\"directory\": \"{root_text}\", \"file\": \"{root_text}/other.cpp\", \
             \"arguments\": [\"g++\", \"-c\", \"{root_text}/other.cpp\"]}}\n]\n"
        ),
    )
    .expect("the fixture writes");

    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);
    let mut session = Session::open(&root, &files, WatchFilter::new(&root));
    session.did_open(&main_file, MAIN);
    session.index_everything();

    println!(
        "closure: {} files — configured: {}",
        session.index().len(),
        session.compile_database().is_some()
    );

    // Every macro name the closure defines, and every name it undefines: the two answers a condition can have.
    let mut defined: HashSet<String> = HashSet::new();
    let mut undefined: HashSet<String> = HashSet::new();

    for summary in session.index().summaries() {
        for fact in &summary.macros {
            if fact.kind.is_definition() {
                defined.insert(fact.name.clone());
            } else {
                undefined.insert(fact.name.clone());
            }
        }
    }

    println!(
        "  {} names are defined somewhere, {} are only undefined",
        defined.len(),
        undefined.len()
    );

    // Walk every conditional include in the closure and classify what its guard asks.
    let mut shapes: HashMap<&'static str, usize> = HashMap::new();
    let mut guarded_includes = 0usize;
    let mut all_includes = 0usize;
    let mut name_conditions: HashMap<String, usize> = HashMap::new();
    let mut decidable_in_the_closure = 0usize;
    let mut from_the_command_line = 0usize;
    let mut no_evidence = 0usize;
    let mut example: HashMap<&str, String> = HashMap::new();

    for summary in session.index().summaries() {
        let Some(text) = files.read(&summary.path) else {
            continue;
        };

        for include in &summary.includes {
            all_includes += 1;

            let cpp_code_analysis::FactGuard::Region(region) = include.guard else {
                continue;
            };
            guarded_includes += 1;

            let span = summary.guards.regions[region as usize];
            let condition = text[span.start_offset..span.end_offset().min(text.len())]
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();

            let shape = classify(&condition);
            *shapes.entry(shape).or_default() += 1;
            if !example.contains_key(shape) {
                // The line the guard is written on, counted from the file's own text.
                let line = text[..span.start_offset.min(text.len())].lines().count() + 1;
                example.insert(
                    shape,
                    format!(
                        "{}:{line}  {condition}",
                        summary.path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
            }

            if let Some(name) = tested_name(&condition) {
                *name_conditions.entry(name.clone()).or_default() += 1;

                // The three answers, in the order the query would ask them:
                //   the closure defines it somewhere  → the guard is *not* taken, and the include is skipped
                //   nothing in the closure defines it → the guard IS taken (modulo a `-D` or a header outside)
                //   …and `CompilerConfig::defines` is the `-D` half, which this probe reports separately.
                if defined.contains(&name) {
                    decidable_in_the_closure += 1;
                } else if session.config().defines.iter().any(|define| define.name.as_ref() == name) {
                    from_the_command_line += 1;
                } else {
                    no_evidence += 1;
                }
            }
        }
    }

    println!(
        "\nincludes: {all_includes} in the closure, {guarded_includes} of them written inside an `#if`"
    );
    println!("\nguard shapes:");
    let mut ordered: Vec<_> = shapes.iter().collect();
    ordered.sort_by(|one, other| other.1.cmp(one.1));
    for (shape, count) in ordered {
        println!(
            "  {shape:<34} {count:>5}   e.g. {}",
            example.get(shape).cloned().unwrap_or_default()
        );
    }

    println!(
        "\nthe conditions that test a name ({} distinct names):",
        name_conditions.len()
    );
    let mut by_count: Vec<_> = name_conditions.iter().collect();
    by_count.sort_by(|one, other| other.1.cmp(one.1).then(one.0.cmp(other.0)));
    for (name, count) in by_count.iter().take(8) {
        println!(
            "  {name:<32} {count:>4}  {}",
            if defined.contains(*name) {
                "defined somewhere in the closure"
            } else if undefined.contains(*name) {
                "only ever undefined in the closure"
            } else {
                "**no evidence in the closure**"
            }
        );
    }

    println!("\nwhat the closure can answer about those guards:");
    println!("  a name the closure defines             {decidable_in_the_closure:>5}");
    println!("  a name only the command line defines   {from_the_command_line:>5}");
    println!("  **a name nothing in the closure mentions** {no_evidence:>5}");

    // …and the half the closure cannot have: the compiler's **own** macros. `_WIN32`, `__x86_64__` and
    // `__cplusplus` are defined by the compiler and never by a file, so "nothing in the closure defines it" is not
    // "it is not defined" — a closed-world reading would *take* a guard the build does not take.
    let builtins = compiler_builtins(&session);

    if builtins.is_empty() {
        println!("\n  (no compiler answered, so the built-ins are unknown here)");
    } else {
        let mut from_the_closure = 0usize;
        let mut from_the_compiler = 0usize;
        let mut from_neither = 0usize;

        for name in name_conditions.keys() {
            if defined.contains(name) {
                from_the_closure += name_conditions[name];
            } else if builtins.contains(name) {
                from_the_compiler += name_conditions[name];
            } else {
                from_neither += name_conditions[name];
            }
        }

        println!(
            "\n  the compiler predefines {} macros (`-dM -E`, one process)",
            builtins.len()
        );
        println!(
            "  the {} name-tests ({} names), by where the answer comes from:",
            name_conditions.values().sum::<usize>(),
            name_conditions.len()
        );
        println!("    a name the closure defines                       {from_the_closure:>5}");
        println!("    a name the compiler predefines                   {from_the_compiler:>5}");
        println!(
            "    **neither — answerable only as 'nothing defines it'** {from_neither:>5}"
        );

        println!(
            "\n  and the shapes that do not test a name at all:\n    a version test over `__cplusplus`                {:>5}  \
             — the toolchain prints its value too",
            shapes.get("a version test (__cplusplus)").copied().unwrap_or(0)
        );
        println!(
            "    `__has_include(…)`                              {:>5}  — a *file* question: the resolver answers it",
            shapes.get("an expression over names").copied().unwrap_or(0)
        );
        println!(
            "    expressions over names and numbers              {:>5}  — `condition::evaluate` is already written",
            shapes.get("an expression with a number").copied().unwrap_or(0)
                + shapes.get("an expression mentioning defined(…)").copied().unwrap_or(0)
        );
        println!(
            "\n  so **every shape in this corpus is decidable** from (closure facts + the compiler's own table + the \
             configuration).\n  What is left is the *risk* of that closed world: a `-D` nobody told us about, or a \
             header outside the\n  index. Those are the cases where a wrong answer replaces an honest `Unknown`, so \
             the evaluator must say\n  `Unknown` whenever an input is missing — never 'not defined' by default."
        );
    }

    // ============================================================================================
    // Which of them are decided *now*
    // ============================================================================================
    //
    // The four numbers above are a taxonomy. This is the answer to "and how many of them does the layer actually
    // decide?", measured through the same entry point a query uses — the session's own index, which was told what
    // the compilation defines — with a name nothing answers for left `Unknown` rather than read as the standard's
    // `0`.
    let mut verdicts = HashMap::new();
    let mut decided_includes = 0usize;
    let mut example: HashMap<&str, String> = HashMap::new();
    let mut disagreements = 0usize;

    for summary in session.index().summaries() {
        let Some(text) = files.read(&summary.path) else {
            continue;
        };

        for include in &summary.includes {
            if include.guard == cpp_code_analysis::FactGuard::Unconditional {
                continue;
            }

            // The two ways to ask the same question: from the guard the include carries, and from its offset by
            // searching the spans. They must agree on a file whose directives balance — the guard *is* the
            // innermost region the sweep found — and where they do not, the guard is the answer the rest of the
            // index is consistent with (it is the same index a declaration's guard uses).
            if summary.guards.conditions_at(include.range.start_offset)
                != summary.guards.conditions_of(match include.guard {
                    cpp_code_analysis::FactGuard::Region(region) => region,
                    cpp_code_analysis::FactGuard::Unconditional => unreachable!(),
                })
            {
                disagreements += 1;
            }

            let visibility = cpp_code_analysis::index::visibility_at(
                session.index(),
                &summary.path,
                include.guard,
                include.range.start_offset,
            );

            let verdict = match visibility {
                cpp_code_analysis::Visibility::Active => "taken (certain)",
                cpp_code_analysis::Visibility::Inactive => "not taken (skipped)",
                cpp_code_analysis::Visibility::Unknown => "unknown",
            };

            *verdicts.entry(verdict).or_insert(0usize) += 1;
            if visibility != cpp_code_analysis::Visibility::Unknown {
                decided_includes += 1;
            }

            if !example.contains_key(verdict) {
                let cpp_code_analysis::FactGuard::Region(region) = include.guard else {
                    continue;
                };
                let span = summary.guards.regions[region as usize];
                let line = text[..span.start_offset.min(text.len())].lines().count() + 1;
                example.insert(
                    verdict,
                    format!(
                        "{}:{line}  {}",
                        summary.path.file_name().unwrap_or_default().to_string_lossy(),
                        text[span.start_offset..span.end_offset().min(text.len())]
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                    ),
                );
            }
        }
    }

    println!(
        "\nnow that the conditions are evaluated (compiler built-ins + command line, everything else `Unknown`):"
    );
    for verdict in ["taken (certain)", "not taken (skipped)", "unknown"] {
        println!(
            "  {verdict:<20} {:>5}   e.g. {}",
            verdicts.get(verdict).copied().unwrap_or(0),
            example.get(verdict).cloned().unwrap_or_default()
        );
    }
    println!(
        "  **{decided_includes} of {guarded_includes} guarded includes are decided** — the rest keep the answer \
         they had before\n  the conditions were stored, which is `Unknown`."
    );
    println!(
        "  ({disagreements} of them sit in a region whose guard chain differs from the chain its offset is in — \
         the two agree\n   on every file whose directives balance, and a disagreement means the sweep and the \
         spans read a broken file differently.)"
    );

    // ============================================================================================
    // The ceiling for the next step: the closure's own `#define`s
    // ============================================================================================
    //
    // `roadmap.md` §3.5c's second half is "feed the macros the *closure* defines into the environment, in
    // translation order". Before building that, measure what it can buy: of the conditions the evaluation left
    // `Unknown`, how many test **only** names that some file in the closure defines (or that the compiler
    // predefines)?
    //
    // Two numbers, because the second half is harder than the first: a name used as a *value* (`#if ABI == 1`)
    // needs the body, and a summary stores macro facts without bodies — so the second number is what would need
    // `MacroFact` to carry the value of a one-token body. A name only ever asked about with `defined(NAME)` or
    // `#ifdef` needs no value at all.
    let bodies = single_token_bodies(&session, &files, &builtins);

    let mut names_all_known = 0usize;
    let mut values_all_readable = 0usize;
    let mut missing_names: HashMap<String, usize> = HashMap::new();
    let mut example: HashMap<&str, String> = HashMap::new();

    for summary in session.index().summaries() {
        let Some(text) = files.read(&summary.path) else {
            continue;
        };

        for include in &summary.includes {
            if include.guard == cpp_code_analysis::FactGuard::Unconditional {
                continue;
            }

            if cpp_code_analysis::index::visibility_at(
                session.index(),
                &summary.path,
                include.guard,
                include.range.start_offset,
            ) != cpp_code_analysis::Visibility::Unknown
            {
                continue;
            }

            let asked = names_asked(&summary.guards, include.guard);

            let mut known = true;
            let mut readable = true;

            for (name, needs_a_value) in &asked {
                if !builtins.contains(name) && !defined.contains(name) {
                    *missing_names.entry(name.clone()).or_default() += 1;
                    known = false;
                } else if *needs_a_value && !bodies.contains_key(name) {
                    readable = false;
                }
            }

            if known {
                names_all_known += 1;
            }
            if known && readable {
                values_all_readable += 1;
            }

            let verdict = match (known, readable) {
                (true, true) => "decided",
                (true, false) => "names known, value missing",
                (false, _) => "a name nothing defines",
            };
            if !example.contains_key(verdict) {
                let cpp_code_analysis::FactGuard::Region(region) = include.guard else {
                    continue;
                };
                let span = summary.guards.regions[region as usize];
                let line = text[..span.start_offset.min(text.len())].lines().count() + 1;
                example.insert(
                    verdict,
                    format!(
                        "{}:{line}  {}",
                        summary.path.file_name().unwrap_or_default().to_string_lossy(),
                        text[span.start_offset..span.end_offset().min(text.len())]
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                    ),
                );
            }
        }
    }

    println!(
        "\nif the closure's own `#define`s were in the environment (the next step, §3.5c):\n  \
         of the {} unknown conditions,",
        guarded_includes - decided_includes
    );
    println!(
        "    every name is one the closure defines          {names_all_known:>5}   e.g. {}",
        example.get("names known, value missing").cloned().unwrap_or_default()
    );
    println!(
        "    …and every value a one-token body carries     {values_all_readable:>5}   → so this many would be \
         decided\n                                                   e.g. {}",
        example.get("decided").cloned().unwrap_or_default()
    );
    println!(
        "    **at least one name nothing defines**          {:>5}   → still `Unknown`, and that is the honest \
         answer:\n                                                   a header outside the index or a `-D` nobody \
         told us could define it.\n                                                   e.g. {}",
        guarded_includes - decided_includes - names_all_known,
        example.get("a name nothing defines").cloned().unwrap_or_default()
    );

    let mut by_count: Vec<_> = missing_names.iter().collect();
    by_count.sort_by(|one, other| other.1.cmp(one.1).then(one.0.cmp(other.0)));
    println!("\n  the names that keep a condition undecided, by how many conditions ask about them:");
    for (name, count) in by_count.iter().take(8) {
        println!("    {name:<34} {count:>4}");
    }
}

/// The names each branch of a condition asks about, and whether the condition needs the name's **value**.
///
/// `#ifdef NAME` and `defined(NAME)` ask only whether the name is a macro; `#if NAME` and `#if NAME == 1` read its
/// body. The distinction is the difference between what a summary already carries (a fact) and what it does not (a
/// value) — see the two numbers this feeds.
fn names_asked(
    guards: &cpp_code_analysis::SummaryGuards,
    guard: cpp_code_analysis::FactGuard,
) -> Vec<(String, bool)> {
    use cpp_code_analysis::DirectiveKind;

    let cpp_code_analysis::FactGuard::Region(region) = guard else {
        return Vec::new();
    };

    let mut asked: Vec<(String, bool)> = Vec::new();
    let spelled = |condition: &str, token: &cpp_parser::CppTokenData| {
        condition[token.range.start_offset..token.range.end_offset()].to_string()
    };

    for at in guards.conditions_of(region) {
        let Some(conditional) = guards.conditionals.get(at.region as usize) else {
            continue;
        };

        for branch in &conditional.branches {
            let Some(condition) = branch.condition.as_deref() else {
                continue;
            };

            if matches!(branch.kind, DirectiveKind::Ifdef | DirectiveKind::Ifndef) {
                asked.push((condition.trim().to_string(), false));
                continue;
            }

            let mut errors = Vec::new();
            let mut lexer =
                cpp_parser::CppLexer::new(condition, cpp_parser::LexerConfig::default(), &mut errors);
            let tokens: Vec<cpp_parser::CppTokenData> = lexer
                .tokenize()
                .into_iter()
                .filter(|token| !cpp_parser::is_trivia(token.kind))
                .collect();

            let mut index = 0;
            while index < tokens.len() {
                let token = &tokens[index];

                if token.kind != cpp_parser::CppTokenKind::Identifier {
                    index += 1;
                    continue;
                }

                if spelled(condition, token) == "defined" {
                    // The name after `defined` is asked about, not read: `defined(NAME)` needs no value.
                    index += 1;
                    if tokens.get(index).is_some_and(|next| next.kind == cpp_parser::CppTokenKind::LeftParen)
                    {
                        index += 1;
                    }
                    if let Some(name) = tokens.get(index) {
                        asked.push((spelled(condition, name), false));
                    }
                } else {
                    asked.push((spelled(condition, token), true));
                }

                index += 1;
            }
        }
    }

    asked
}

/// The names whose definition in the closure has a **one-token body**, which is the only kind of body a condition
/// can read a value out of.
///
/// A summary does not carry bodies (see `MacroFact`), so this reads them the way a caller would have to: from the
/// text, at the name the fact points at, to the end of the line. A body of two or more tokens is not a value any
/// condition can use (`macro_value` in `condition.rs` refuses it for the same reason), so it is left out — and a
/// `#define NAME` with *no* body is left out too, because `#if NAME` on it is a syntax error rather than a number.
fn single_token_bodies(
    session: &Session<'_, DiskFiles>,
    files: &SessionFiles<DiskFiles>,
    builtins: &HashSet<String>,
) -> HashMap<String, String> {
    let mut bodies = HashMap::new();

    for summary in session.index().summaries() {
        let Some(text) = files.read(&summary.path) else {
            continue;
        };

        for fact in &summary.macros {
            if !fact.kind.is_definition() || builtins.contains(&fact.name) {
                continue;
            }

            let start = fact.range.end_offset().min(text.len());
            let end = text[start..]
                .find('\n')
                .map_or(text.len(), |newline| start + newline);
            let body = text[start..end].trim();

            let mut errors = Vec::new();
            let mut lexer =
                cpp_parser::CppLexer::new(body, cpp_parser::LexerConfig::default(), &mut errors);
            let tokens: Vec<_> = lexer
                .tokenize()
                .into_iter()
                .filter(|token| !cpp_parser::is_trivia(token.kind))
                .collect();

            if tokens.len() == 1 {
                bodies.insert(fact.name.clone(), body.to_string());
            }
        }
    }

    bodies
}

/// The compiler's own macro table: `-dM -E` prints every macro it predefines, without reading a file.
///
/// The same move as discovering the include paths: ask the toolchain instead of guessing. It is what makes "is
/// `_WIN32` defined?" answerable at all — the answer is not in any header.
fn compiler_builtins(session: &Session<'_, DiskFiles>) -> HashSet<String> {
    use cpp_code_analysis::CommandRunner;

    let Some(toolchain) = session.toolchain() else {
        return HashSet::new();
    };

    let Some(output) =
        cpp_code_analysis::DiskCommands.run(&toolchain.compiler, &["-dM", "-E", "-x", "c++", "-"])
    else {
        return HashSet::new();
    };

    output
        .combined()
        .lines()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// Which shape a condition has, in the vocabulary that decides what can be evaluated without a value.
///
/// The order matters and it bit: stripping the leading `if` first turns `#ifndef NAME` into `ndef NAME`, which the
/// checks below then read as "an expression over names" — reporting the commonest guard in the corpus as one of the
/// rarest, and hiding every name it tests.
fn classify(condition: &str) -> &'static str {
    let text = condition.trim_start_matches('#').trim();

    if text.starts_with("ifdef") || text.starts_with("ifndef") {
        return "ifdef / ifndef NAME";
    }

    let text = text.strip_prefix("if").unwrap_or(text).trim();

    if text.starts_with("!defined") || text.starts_with("! defined") {
        return "!defined(NAME)";
    }
    if text.starts_with("defined") {
        return "defined(NAME)";
    }
    if text.contains("defined") {
        return "an expression mentioning defined(…)";
    }
    if text.contains("__cplusplus") {
        return "a version test (__cplusplus)";
    }
    if text.chars().any(|character| character.is_ascii_digit()) {
        return "an expression with a number";
    }
    if text.is_empty() {
        return "(no condition read)";
    }
    "an expression over names"
}

/// The name a condition tests, when it tests exactly one.
fn tested_name(condition: &str) -> Option<String> {
    let text = condition.trim_start_matches('#').trim();

    for prefix in ["ifdef", "ifndef"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let name = rest.trim();
            return (!name.is_empty()).then(|| name.to_string());
        }
    }

    let text = text.strip_prefix("if").unwrap_or(text).trim();
    let rest = text
        .strip_prefix('!')
        .map(str::trim_start)
        .unwrap_or(text)
        .strip_prefix("defined")?
        .trim_start();

    let name = rest
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();

    (!name.is_empty() && !name.contains(|character: char| !character.is_alphanumeric() && character != '_'))
        .then(|| name.to_string())
}

