//! Probe: **how much of the remaining parse failure is "a macro whose shape nobody can know without expanding it"?**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example macro_shape -- <file list>
//! ```
//!
//! # The question
//!
//! Every round of parser work has ended on the same sentence: the failures that are left are lines with macros on
//! them, and a macro is expanded before the grammar runs. But "there is a macro on the line" is not yet an answer to
//! *what to build next*, because there are three different fixes for three different situations:
//!
//! ```text
//! the macro is defined in this file, and its body has a shape the grammar has a word for
//!     → the *index* could tell the parser (a table), no expansion needed
//! the macro is defined in another file of the closure, same story
//!     → the same table, filled from the closure instead of from one file
//! the macro's body has no shape the grammar can use — it opens a scope, it *is* the declaration, it is a
//! statement that brings its own `;`
//!     → nothing but expansion settles this, and this is what a second reading costs
//! ```
//!
//! So this probe measures the four numbers that decide between them, on one corpus:
//!
//! ```text
//! 1. of the failing files, how many have a first error whose line mentions a macro at all
//! 2. …of those, how many mention a macro whose shape the index already knows (Specifier/Statement/Type/…)
//! 3. …and how many mention one whose shape is `Unknown` — the family that only expansion can settle
//! 4. **what a table would buy**: re-parse every file with a `SymbolMap` holding every macro shape the closure
//!    knows, and count clean before/after — plus the number that matters, files that became **dirty**
//! ```
//!
//! Number 4 is the one with teeth. A `SymbolTable` is asked `kind_of(name)` with **no position**, so one table for
//! a whole closure is an approximation with a known failure mode (`docs/index-design.md`, "索引与 parser 的关系：
//! 先不接"): a name that is a macro in one header and something else in another gets one answer for both. The
//! "became dirty" count is that failure mode measured rather than argued about.

use std::collections::HashMap;
use std::path::PathBuf;

use cpp_parser::{CppLexer, CppParser, CppTokenKind, LexerConfig, LineIndex, MacroBody, ParserConfig};

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

    // What the index knows about every macro in the closure: its shape, and whether it is invoked with arguments.
    // A name defined in two files with two different shapes is *not* a detail — it is the reason a position-less
    // table cannot be the whole answer — so the conflicts are counted rather than averaged away.
    let mut closure: HashMap<String, (bool, MacroBody)> = HashMap::new();
    let mut conflicts: HashMap<String, Vec<MacroBody>> = HashMap::new();
    let mut own: HashMap<PathBuf, HashMap<String, (bool, MacroBody)>> = HashMap::new();

    let mut texts: HashMap<PathBuf, String> = HashMap::new();

    for path in &paths {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };

        let summary = cpp_code_analysis::summarize(path, &source, key);
        let mut mine: HashMap<String, (bool, MacroBody)> = HashMap::new();

        for fact in summary.macros.iter().filter(|fact| fact.kind.is_definition()) {
            let shape = (fact.function_like, fact.body);
            mine.insert(fact.name.clone(), shape);

            match closure.get(&fact.name) {
                Some(known) if *known != shape => {
                    let seen = conflicts.entry(fact.name.clone()).or_default();
                    for candidate in [known.1, shape.1] {
                        if !seen.contains(&candidate) {
                            seen.push(candidate);
                        }
                    }
                }
                Some(_) => {}
                None => {
                    closure.insert(fact.name.clone(), shape);
                }
            }
        }

        own.insert(path.clone(), mine);
        texts.insert(path.clone(), source);
    }

    let conflicting: Vec<(&String, &Vec<MacroBody>)> = conflicts
        .iter()
        .filter(|(_, shapes)| shapes.len() > 1)
        .collect();

    println!(
        "files {} | macros the closure defines {} | names with more than one shape {}",
        texts.len(),
        closure.len(),
        conflicting.len()
    );
    let mut shown: Vec<_> = conflicting.iter().collect();
    shown.sort_by_key(|(_, shapes)| std::cmp::Reverse(shapes.len()));
    for (name, shapes) in shown.iter().take(6) {
        let mut list: Vec<String> = shapes.iter().map(|shape| format!("{shape:?}")).collect();
        list.sort();
        println!("  conflicting shape  {name:<34} {list:?}");
    }

    // ── the census ───────────────────────────────────────────────────────────────────────────────
    let mut families: HashMap<&'static str, usize> = HashMap::new();
    let mut examples: HashMap<&'static str, String> = HashMap::new();
    // The same question asked with a three-line window, because a diagnostic often lands a line late: the two
    // numbers together say how much of this measurement is the window rather than the corpus.
    let mut windowed = 0usize;
    let mut clean = 0usize;
    let mut failing = 0usize;
    let mut names_on_lines: HashMap<String, usize> = HashMap::new();

    for path in &paths {
        let Some(source) = texts.get(path) else {
            continue;
        };

        let tree = CppParser::parse(source, ParserConfig::default());
        let errors = tree.get_errors();

        if errors.is_empty() {
            clean += 1;
            continue;
        }

        failing += 1;

        let Some((line, _)) = LineIndex::parse(source).get_line_col(errors[0].range.start(), source) else {
            continue;
        };

        // The identifiers written on the line the first error is on. A macro that breaks a declaration is usually
        // on that line — sometimes the one above, which is why the *same* classification is also run over a
        // three-line window below: the two numbers together say how much of this is the window rather than the
        // corpus.
        let (line_start, line_end) = line_bounds(source, line);
        let (window_from, _) = line_bounds(source, line.saturating_sub(2));
        let mut errors_ = Vec::new();
        let mut lexer = CppLexer::new(source, LexerConfig::default(), &mut errors_);
        let tokens = lexer.tokenize();
        let spelled = |range: &cpp_parser::SourceRange| source[range.start_offset..range.end_offset()].to_string();
        let names_in = |from: usize, to: usize, spelled: &dyn Fn(&cpp_parser::SourceRange) -> String| {
            let mut names: Vec<String> = tokens
                .iter()
                .filter(|token| token.kind == CppTokenKind::Identifier)
                .filter(|token| token.range.start_offset >= from && token.range.start_offset < to)
                .map(|token| spelled(&token.range))
                .collect();
            names.sort();
            names.dedup();
            names
        };

        let on_the_line = names_in(line_start, line_end, &spelled);

        let mine = own.get(path);
        let is_a_macro = |name: &String| mine.and_then(|mine| mine.get(name)).or_else(|| closure.get(name));

        if names_in(window_from, line_end, &spelled)
            .iter()
            .any(|name| is_a_macro(name).is_some())
        {
            windowed += 1;
        }

        let mut saw_a_macro = false;
        let mut shape_here = false;
        let mut shape_in_the_closure = false;
        let mut shape_unknown = false;

        for name in &on_the_line {
            let known = mine.and_then(|mine| mine.get(name)).or_else(|| closure.get(name));
            let Some((_, shape)) = known else {
                continue;
            };

            saw_a_macro = true;
            *names_on_lines.entry(name.clone()).or_default() += 1;

            match shape {
                MacroBody::Unknown => shape_unknown = true,
                _ if mine.is_some_and(|mine| mine.contains_key(name)) => shape_here = true,
                _ => shape_in_the_closure = true,
            }
        }

        let family = match (saw_a_macro, shape_unknown, shape_here || shape_in_the_closure) {
            (false, _, _) => "no macro on the line",
            (true, true, _) => "a macro whose shape is Unknown",
            (true, false, true) => "a macro whose shape is known",
            // A macro with no shape at all cannot happen: `Unknown` is the shape of "the table knows the name".
            (true, false, false) => "a macro, shape unclassified",
        };

        *families.entry(family).or_default() += 1;
        examples.entry(family).or_insert_with(|| {
            format!(
                "{}:{}  {}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                line + 1,
                source
                    .lines()
                    .nth(line)
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(76)
                    .collect::<String>()
            )
        });
    }

    println!("\n--- what the first error of a failing file is about ({failing} failures) ---");
    for family in [
        "no macro on the line",
        "a macro whose shape is known",
        "a macro whose shape is Unknown",
    ] {
        let count = families.get(family).copied().unwrap_or(0);
        println!(
            "  {family:<34} {count:>5}  ({:>4.0}%)   e.g. {}",
            count as f64 * 100.0 / failing.max(1) as f64,
            examples.get(family).cloned().unwrap_or_default()
        );
    }
    println!(
        "  (with a three-line window — the error line and the two above it — {} of {failing} mention a macro at \
         all, so the strict count above is a floor)",
        windowed
    );

    let mut ranked: Vec<(&String, &usize)> = names_on_lines.iter().collect();
    ranked.sort_by(|one, other| other.1.cmp(one.1));
    println!("\n--- the macros those lines mention, most often first ---");
    for (name, count) in ranked.iter().take(10) {
        let shape = closure.get(*name).map(|(function_like, body)| {
            format!(
                "{}{body:?}",
                if *function_like { "function-like " } else { "" }
            )
        });
        println!(
            "  {name:<34} {count:>4}  {}",
            shape.unwrap_or_else(|| "not a macro in the closure".to_string())
        );
    }

    // ── what a table would buy ───────────────────────────────────────────────────────────────────
    //
    // The experiment the rest of this probe exists for: give the parser *every* macro shape the closure knows, as
    // one position-less table, and re-parse. Two numbers come out, and the second is the price of the
    // approximation: a file that used to parse cleanly and no longer does.
    //
    // Twice: once with **every** name, and once with only the names whose shape is unambiguous in the closure. The
    // gap between the two runs says how much of the damage is the table being *incoherent* (one name, two shapes,
    // one answer) rather than the table being *knowledge out of position*.
    let mut all = cpp_parser::SymbolMap::new();
    let mut unambiguous = cpp_parser::SymbolMap::new();

    for (name, (function_like, body)) in &closure {
        let kind = cpp_parser::SymbolKind::Macro {
            function_like: *function_like,
            body: *body,
        };

        all.insert(name, kind);

        if !conflicts.contains_key(name) {
            unambiguous.insert(name, kind);
        }
    }

    for (label, symbols) in [("every name", &all), ("only unambiguous names", &unambiguous)] {
        let mut clean_fed = 0usize;
        let mut became_clean: Vec<String> = Vec::new();
        let mut became_dirty: Vec<(String, String)> = Vec::new();
        let mut messages_plain = 0usize;
        let mut messages_fed = 0usize;

        for path in &paths {
            let Some(source) = texts.get(path) else {
                continue;
            };

            let plain_tree = CppParser::parse(source, ParserConfig::default());
            let plain = plain_tree.get_errors();
            let config = ParserConfig::default().with_symbol_table(symbols);
            let fed_tree = CppParser::parse(source, config);
            let fed = fed_tree.get_errors();

            messages_plain += plain.len();
            messages_fed += fed.len();

            if fed.is_empty() {
                clean_fed += 1;
            }

            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            match (plain.is_empty(), fed.is_empty()) {
                (false, true) => became_clean.push(name),
                (true, false) => {
                    // Where the *new* error lands, and on what — the mechanism of the regression, which is the
                    // whole reason this experiment exists: the table is asked `kind_of(name)` with no position, so
                    // a name that is a macro in one header and a function in another gets one answer for both.
                    let where_and_what = fed
                        .first()
                        .and_then(|error| {
                            LineIndex::parse(source)
                                .get_line_col(error.range.start(), source)
                                .map(|(line, _)| (error, line))
                        })
                        .map(|(error, line)| {
                            format!(
                                "{}:{} {}",
                                line + 1,
                                error.message,
                                source
                                    .lines()
                                    .nth(line)
                                    .unwrap_or("")
                                    .trim()
                                    .chars()
                                    .take(52)
                                    .collect::<String>()
                            )
                        })
                        .unwrap_or_default();

                    became_dirty.push((name, where_and_what));
                }
                _ => {}
            }
        }

        println!("\n--- and if the parser were told the macro shapes of the closure ({label}) ---");
        println!("  clean                      {clean} → {clean_fed}");
        println!("  messages                   {messages_plain} → {messages_fed}");
        println!(
            "  files that became clean    {}   (the table's upside)",
            became_clean.len()
        );
        for name in became_clean.iter().take(8) {
            println!("      + {name}");
        }
        println!(
            "  **files that became dirty** {}   (the price of one table for a whole closure)",
            became_dirty.len()
        );
        for (name, what) in became_dirty.iter().take(8) {
            println!("      - {name:<26} {what}");
        }

        // Which of the table's names the first dirty file mentions: the candidates for the misreading, with the
        // shape that was handed over. A name whose only "evidence" is another file's `#define` is exactly the
        // position-less approximation, made visible.
        let first_dirty = became_dirty.first().and_then(|(dirty, _)| {
            paths
                .iter()
                .find(|path| path.file_name().is_some_and(|held| held.to_string_lossy() == *dirty))
                .and_then(|path| texts.get(path).map(|source| (dirty, source)))
        });

        if let Some((dirty, source)) = first_dirty {
            let mut mentioned: Vec<String> = closure
                .iter()
                .filter(|(name, _)| source.contains(name.as_str()))
                .map(|(name, (function_like, body))| {
                    format!(
                        "{name}({}{body:?})",
                        if *function_like { "function-like, " } else { "" }
                    )
                })
                .collect();
            mentioned.sort();
            mentioned.truncate(8);
            println!("      {dirty} mentions: {mentioned:?}");
        }
    }
}

/// The byte range of a zero-based line.
fn line_bounds(source: &str, line: usize) -> (usize, usize) {
    let mut start = 0usize;

    for (index, text) in source.split_inclusive('\n').enumerate() {
        if index == line {
            return (start, start + text.len());
        }

        start += text.len();
    }

    (source.len(), source.len())
}
