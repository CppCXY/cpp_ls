//! Probe: **what would our own bounded macro expansion buy?**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example macro_expansion -- <list> [--steps N]
//! ```
//!
//! # The question
//!
//! Measured with the real compiler as ground truth (`cl /E` on the same headers, then the same parser): the MSVC
//! closure reads with **255** messages across 109 files, and its fully expanded text reads with **62** — so most of
//! what the shape rules have been chasing is *expansion*, not grammar. Before building an expander into the
//! parser, this probe asks the cheaper question: **how much of that does our own bounded expansion get, and what
//! does it cost?**
//!
//! # What it does, and what it deliberately does not
//!
//! For every file of the closure it builds the macro definitions the index already holds — name, parameters, and
//! the replacement list sliced out of the file that wrote it — and expands the file's **own text** for the macros
//! whose bodies are known. Then it parses both readings and prints them side by side.
//!
//! ```text
//! ✔ object-like and function-like invocations, arguments substituted by name
//! ✔ bounded: a step count, so a recursive macro cannot run away
//! ✔ directives are left alone (nothing is expanded inside a `#define`, and `defined(X)` is not a call)
//! ✘ no `#` and no `##` — a body carrying either is skipped rather than expanded wrongly
//! ✘ no condition evaluation: the definitions are the whole closure's, and which of them is *in force* at an
//!   offset is the condition layer's question (`ProjectIndex::macros_at`), not this probe's
//! ✘ no positions: the expanded text is a *reading*, and mapping every token back to the file it was written in is
//! the part a product version would have to add
//! ```
//!
//! The raw reading here is **without** the closure's evidence, which is deliberately the strictest baseline: the
//! question is "can expansion replace that evidence", and the census's evidence-backed number is 82 clean / 255
//! messages over the same files.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use cpp_code_analysis::{CompilerConfig, DiskFiles, FileProvider, IncludeBudget, SummaryStore};
use cpp_parser::{CppLexer, CppParser, CppTokenKind, LexerConfig, ParserConfig};

/// One macro this probe is willing to expand.
struct Definition {
    /// Empty for an object-like macro.
    parameters: Vec<String>,
    /// The replacement list, as the file that defines it wrote it.
    body: String,
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let Some(list) = arguments.iter().skip(1).find(|one| !one.starts_with("--")) else {
        eprintln!("Usage: macro_expansion <list> [--steps N]");
        return;
    };
    let steps: usize = arguments
        .iter()
        .position(|one| one == "--steps")
        .and_then(|at| arguments.get(at + 1))
        .and_then(|count| count.parse().ok())
        .unwrap_or(2);

    let text = std::fs::read_to_string(list).expect("the list reads");
    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let started = std::time::Instant::now();
    let root = std::env::temp_dir().join("cppls-macro-expansion");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");

    // The corpus is its own search path — the same approximation `std_probe` documents, sorted for the same
    // reason (a `HashSet` here made the include facts a matter of the hash seed).
    let files = DiskFiles;
    let mut config = CompilerConfig::default();
    let mut directories: BTreeSet<PathBuf> = BTreeSet::new();
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

    let mut store = SummaryStore::open(&root, config);
    for path in &paths {
        store.index_includes_from(path, IncludeBudget::default());
    }

    // Every definition the closure holds, by name — and only the ones this probe can trust:
    //
    // ```text
    // one unconditional #define, and no #undef anywhere in the closure      -> expandable
    // two definitions of the same name, or a definition inside an #if       -> not expanded
    // a name any file #undefs                                              -> not expanded
    // ```
    //
    // That filter is the whole lesson of the first run: expanding with *the last definition read* took the closure
    // from 938 messages to **1177** and cost 5 clean files, because a name defined differently under two branches
    // (or undefined again later) was expanded with a body that was not in force — `corecrt_wctype.h` 0 → 32,
    // `stat.h` 0 → 12. The preprocessor's answer is per position, and until this probe asks per position (the
    // index's own `macros_at` walk does, one walk per offset) the only sound reading is "expand what cannot be
    // ambiguous".
    let mut unconditional: HashMap<String, Definition> = HashMap::new();
    let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut undone: std::collections::HashSet<String> = std::collections::HashSet::new();

    for summary in store.index().summaries() {
        let Some(source) = files.read(&summary.path) else {
            continue;
        };
        for fact in &summary.macros {
            if !fact.kind.is_definition() {
                undone.insert(fact.name.clone());
                continue;
            }
            if fact.guard != cpp_code_analysis::FactGuard::Unconditional {
                ambiguous.insert(fact.name.clone());
                continue;
            }

            let body = match fact.body_range {
                // A `#define` with an empty replacement list: the macro is real and expands to nothing, which is
                // the one definition that needs no text.
                None => String::new(),
                Some(range) => source
                    .get(range.start_offset..range.end_offset())
                    .unwrap_or("")
                    .to_string(),
            };
            let definition = Definition {
                parameters: parameters_of(&source, fact, fact.body_range.map(|r| r.start_offset)),
                body,
            };

            if unconditional.insert(fact.name.clone(), definition).is_some() {
                ambiguous.insert(fact.name.clone());
            }
        }
    }

    for name in &ambiguous {
        unconditional.remove(name);
    }
    for name in &undone {
        unconditional.remove(name);
    }
    let definitions = unconditional;

    println!(
        "closure: {} files indexed in {:?}, {} macros with a body",
        store.index().len(),
        started.elapsed(),
        definitions.len()
    );

    let (mut raw_errors, mut expanded_errors) = (0usize, 0usize);
    let (mut raw_clean, mut expanded_clean) = (0usize, 0usize);
    let mut improved: Vec<(String, usize, usize)> = Vec::new();
    let mut worse: Vec<(String, usize, usize)> = Vec::new();

    for path in &paths {
        let Some(source) = files.read(path) else {
            continue;
        };
        let expanded = expand(&source, &definitions, steps);
        let raw = CppParser::parse(&source, ParserConfig::default()).get_errors().len();
        let after = CppParser::parse(&expanded, ParserConfig::default())
            .get_errors()
            .len();

        raw_errors += raw;
        expanded_errors += after;
        if raw == 0 {
            raw_clean += 1;
        }
        if after == 0 {
            expanded_clean += 1;
        }
        if after < raw {
            improved.push((name_of(path), raw, after));
        }
        if after > raw {
            worse.push((name_of(path), raw, after));
        }
    }

    println!(
        "\nover {} files, {steps} steps:\n  raw      {raw_clean:>3} clean, {raw_errors:>4} messages\n  \
         expanded {expanded_clean:>3} clean, {expanded_errors:>4} messages",
        paths.len()
    );
    println!("\nfiles that improved ({}):", improved.len());
    for (file, raw, after) in &improved {
        println!("  {file:<28} {raw:>3} -> {after:>3}");
    }
    if !worse.is_empty() {
        println!("\n**files that got worse** ({}):", worse.len());
        for (file, raw, after) in &worse {
            println!("  {file:<28} {raw:>3} -> {after:>3}");
        }
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// The parameter names of a function-like macro, read from the gap between its name and its replacement list.
///
/// The facts carry the two ends and not the list itself (`MacroFact::body_range`'s documentation says why), and the
/// gap is exactly `(a, b, c)` — so the list is read from the text the file already wrote rather than stored twice.
fn parameters_of(source: &str, fact: &cpp_code_analysis::MacroFact, body_start: Option<usize>) -> Vec<String> {
    if !fact.function_like {
        return Vec::new();
    }
    let from = fact.range.end_offset();
    let to = body_start.unwrap_or(source.len()).min(source.len());
    let Some(gap) = source.get(from..to) else {
        return Vec::new();
    };

    let mut parameters = Vec::new();
    let mut depth = 0isize;
    let mut current = String::new();
    for character in gap.chars() {
        match character {
            '(' => {
                depth += 1;
                current.clear();
            }
            ')' => {
                if !current.trim().is_empty() {
                    parameters.push(current.trim().to_string());
                }
                break;
            }
            ',' if depth == 1 => {
                if !current.trim().is_empty() {
                    parameters.push(current.trim().to_string());
                }
                current.clear();
            }
            _ if depth >= 1 => current.push(character),
            _ => {}
        }
    }

    parameters
}

/// Expand every invocation whose body this probe has, bounded by `steps`.
///
/// One pass over the lexed tokens, collecting `(range, text)` replacements and applying them right to left, so
/// offsets stay valid while the list is built. A body that carries `#` or `##` is **not** expanded: stringizing and
/// pasting are a different reading, and doing them half-way would produce text nobody wrote.
fn expand(source: &str, definitions: &HashMap<String, Definition>, steps: usize) -> String {
    if steps == 0 || definitions.is_empty() {
        return source.to_string();
    }

    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(source, LexerConfig::default(), &mut errors);
    let tokens = lexer.tokenize();

    let directives = directive_lines(source);
    let significant: Vec<usize> = (0..tokens.len())
        .filter(|at| !is_trivia(tokens[*at].kind))
        .collect();

    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    let mut position = 0usize;

    while position < significant.len() {
        let at = significant[position];
        let token = &tokens[at];
        let (start, end) = (token.range.start_offset, token.range.end_offset());

        if token.kind != CppTokenKind::Identifier || inside_a_directive(&directives, start) {
            position += 1;
            continue;
        }

        // `defined(X)` is the preprocessor's own operator, not an invocation of a macro named `defined`.
        if position > 0 {
            let before = range_of(&tokens[significant[position - 1]]);
            if source
                .get(before.start_offset..before.end_offset())
                .is_some_and(|word| word == "defined")
            {
                position += 1;
                continue;
            }
        }

        let name = &source[start..end];
        let Some(definition) = definitions.get(name) else {
            position += 1;
            continue;
        };

        if definition.parameters.is_empty() {
            let body = expand(&definition.body, definitions, steps - 1);
            replacements.push((start, end, body));
            position += 1;
            continue;
        }

        // A function-like macro is only an invocation with arguments after it. Without a `(` the name is the
        // ordinary identifier the preprocessor would leave alone.
        let Some(open_at) = significant.get(position + 1).copied() else {
            position += 1;
            continue;
        };
        if tokens[open_at].kind != CppTokenKind::LeftParen {
            position += 1;
            continue;
        }

        let Some(close_at) = matching_paren(&tokens, &significant, position + 1) else {
            position += 1;
            continue;
        };

        let arguments = split_arguments(source, &tokens, &significant, position + 1, close_at);
        let invocation_end = range_of(&tokens[significant[close_at]]).end_offset();

        if carries_pasting(&definition.body) {
            position = close_at + 1;
            continue;
        }

        let substituted = substitute(&definition.body, &definition.parameters, &arguments);
        let body = expand(&substituted, definitions, steps - 1);
        replacements.push((start, invocation_end, body));
        position = close_at + 1;
    }

    let mut result = source.to_string();
    for (start, end, body) in replacements.into_iter().rev() {
        result.replace_range(start..end, &body);
    }

    result
}

fn range_of(token: &cpp_parser::CppTokenData) -> cpp_parser::SourceRange {
    token.range
}

fn is_trivia(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace
            | CppTokenKind::Newline
            | CppTokenKind::LineComment
            | CppTokenKind::BlockComment
    )
}

/// The byte ranges of the lines that begin with a `#`, continuations included.
fn directive_lines(source: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0usize;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let start = offset + (line.len() - trimmed.len());
            let mut end = offset + line.len();
            let mut continues = line.trim_end().ends_with('\\');
            // A directive with a `\` at the end owns the next line too.
            while continues {
                let next = &source[end..];
                let Some(next_line) = next.split_inclusive('\n').next() else {
                    break;
                };
                if next_line.is_empty() {
                    break;
                }
                end += next_line.len();
                continues = next_line.trim_end().ends_with('\\');
            }
            ranges.push((start, end));
        }
        offset += line.len();
    }

    ranges
}

fn inside_a_directive(ranges: &[(usize, usize)], offset: usize) -> bool {
    ranges
        .iter()
        .any(|(start, end)| offset >= *start && offset < *end)
}

/// The index in `significant` of the `)` that closes the `(` at `open`.
fn matching_paren(
    tokens: &[cpp_parser::CppTokenData],
    significant: &[usize],
    open: usize,
) -> Option<usize> {
    let mut depth = 0isize;
    for at in open..significant.len() {
        match tokens[significant[at]].kind {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }

    None
}

/// The arguments of the invocation whose `(` is at `open`, as written — the text between the top-level commas.
fn split_arguments(
    source: &str,
    tokens: &[cpp_parser::CppTokenData],
    significant: &[usize],
    open: usize,
    close: usize,
) -> Vec<String> {
    let (from, to) = (
        range_of(&tokens[significant[open]]).end_offset(),
        range_of(&tokens[significant[close]]).start_offset,
    );
    let Some(text) = source.get(from..to) else {
        return Vec::new();
    };

    let mut arguments = Vec::new();
    let mut depth = 0isize;
    let mut current = String::new();
    for character in text.chars() {
        match character {
            '(' | '[' | '{' => {
                depth += 1;
                current.push(character);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(character);
            }
            ',' if depth == 0 => {
                arguments.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }

    let last = current.trim();
    if !last.is_empty() || !arguments.is_empty() {
        arguments.push(last.to_string());
    }

    arguments
}

/// The body with each parameter replaced by the argument written for it.
///
/// By **name**, token by token: a prefix of a parameter is a different identifier, and replacing by text would
/// rewrite `_Args` inside `_Args2`.
fn substitute(body: &str, parameters: &[String], arguments: &[String]) -> String {
    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(body, LexerConfig::default(), &mut errors);
    let tokens = lexer.tokenize();

    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for token in &tokens {
        if token.kind != CppTokenKind::Identifier {
            continue;
        }
        let (start, end) = (token.range.start_offset, token.range.end_offset());
        let name = &body[start..end];
        if let Some(at) = parameters.iter().position(|parameter| parameter == name)
            && let Some(argument) = arguments.get(at)
        {
            replacements.push((start, end, argument.clone()));
        }
    }

    let mut result = body.to_string();
    for (start, end, argument) in replacements.into_iter().rev() {
        result.replace_range(start..end, &argument);
    }

    result
}

/// Does this body stringize or paste? Then it is not expanded here — see the module documentation.
fn carries_pasting(body: &str) -> bool {
    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(body, LexerConfig::default(), &mut errors);

    lexer.tokenize().iter().any(|token| {
        matches!(token.kind, CppTokenKind::Hash | CppTokenKind::HashHash)
    })
}
