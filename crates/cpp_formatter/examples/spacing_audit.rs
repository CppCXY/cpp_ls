//! **How many same-line gaps does the formatter space the way clang-format does?**
//!
//! P0a of `docs/cpp-format.md` §十: the line structure is still the input's, so a whole-file diff against
//! clang-format says nothing yet (clang-format re-indents and re-breaks). What *can* be measured, and is the
//! decision this step is made of, is the width between two tokens that both sides keep on one line.
//!
//! ```text
//! usage: spacing_audit <clang-format.exe> <file>...
//! ```
//!
//! For each file: format it, format it with clang-format, lex both, and for every gap that is on one line in both
//! outputs and holds no comment, compare the widths. The mismatches are reported by the pair of token kinds that
//! met there — that is the list of rules to write next, in order of how much they cost.

use std::path::Path;
use std::process::Command;

use cpp_formatter::config;
use cpp_formatter::tokens;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: spacing_audit <clang-format.exe> <file>...");
        std::process::exit(2);
    };
    let roots: Vec<String> = arguments.collect();
    if roots.is_empty() {
        eprintln!("spacing_audit: no files or directories");
        std::process::exit(2);
    }
    let mut files: Vec<String> = Vec::new();
    for root in &roots {
        let path = Path::new(root);
        if path.is_dir() {
            collect(path, &mut files);
        } else {
            files.push(root.clone());
        }
    }

    let mut compared = 0usize;
    let mut matching = 0usize;
    let mut tokens_compared = 0usize;
    let mut files_done = 0usize;
    let mut skipped_tokens: Vec<String> = Vec::new();
    let mut mismatches: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for file in &files {
        let path = Path::new(file);
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("spacing_audit: cannot read {file}");
            continue;
        };
        let (style, config_note) = style_for(path);
        if let Some(note) = config_note {
            eprintln!("spacing_audit: {note}");
        }
        let ours = cpp_formatter::format_file(std::path::Path::new(file), &source, &style);

        let Some(theirs) = run(&clang_format, file) else {
            eprintln!("spacing_audit: clang-format failed on {file}");
            continue;
        };

        let our_stream = tokens::stream(&ours);
        let their_stream = tokens::stream(&theirs);
        if our_stream.tokens.len() != their_stream.tokens.len() {
            skipped_tokens.push(format!("{file}: token counts differ"));
            continue;
        }
        let kinds_agree = our_stream
            .tokens
            .iter()
            .zip(&their_stream.tokens)
            .all(|(mine, theirs)| mine.kind == theirs.kind && mine.text == theirs.text);
        if !kinds_agree {
            skipped_tokens.push(format!("{file}: token kinds differ"));
            continue;
        }
        files_done += 1;
        tokens_compared += our_stream.tokens.len();

        // **A `#` line is not compared.** P0a copies directives verbatim (and `docs/cpp-format.md` leaves macro
        // bodies to P4), while clang-format normalises the space after `#` and formats a macro's parameters. Counting
        // those gaps would measure work that is deliberately not done here; measured, it is 3 000+ of the 6 000
        // "mismatches" on the whole project.
        let directive_lines = directive_line_gaps(&our_stream, &their_stream);

        for index in 1..our_stream.tokens.len() {
            if directive_lines[index] {
                continue;
            }
            let mine = &our_stream.gaps[index];
            let others = &their_stream.gaps[index];
            let same_line = mine.newlines == 0
                && others.newlines == 0
                && mine.comments.is_empty()
                && others.comments.is_empty();
            if !same_line {
                continue;
            }
            let ours_width = mine.end.saturating_sub(mine.start).min(8);
            let theirs_width = others.end.saturating_sub(others.start).min(8);
            compared += 1;
            if ours_width == theirs_width {
                matching += 1;
            } else {
                if samples.len() < 12 {
                    samples.push(format!(
                        "{file}:\n      ours:   {}\n      theirs: {}",
                        line_at(&ours, mine.start).trim(),
                        line_at(&theirs, others.start).trim()
                    ));
                }
                let key = format!(
                    "{:?} -> {:?} (ours {ours_width}, clang-format {theirs_width})",
                    our_stream.tokens[index - 1].kind, our_stream.tokens[index].kind
                );
                *mismatches.entry(key).or_default() += 1;
            }
        }
    }

    println!(
        "spacing: {files_done} file(s), {tokens_compared} token(s), {compared} same-line gap(s) compared"
    );
    if compared > 0 {
        println!(
            "matching: {matching} of {compared} ({:.2}%)",
            matching as f64 * 100.0 / compared as f64
        );
    }
    if !skipped_tokens.is_empty() {
        println!("skipped {} file(s):", skipped_tokens.len());
        for line in skipped_tokens.iter().take(10) {
            println!("  {line}");
        }
    }
    if !samples.is_empty() {
        println!("first mismatches:");
        for sample in samples.iter().take(12) {
            println!("  {sample}");
        }
    }
    if !mismatches.is_empty() {
        let mut ranked: Vec<(&String, &usize)> = mismatches.iter().collect();
        ranked.sort_by(|left, right| right.1.cmp(left.1));
        println!("mismatches by pair (top 15):");
        for (key, count) in ranked.iter().take(15) {
            println!("  {count:>6}  {key}");
        }
    }
}

/// The style that applies to `file`: the nearest `.clang-format`, else LLVM.
/// The style that applies to `file`: the nearest `.clang-format`, else LLVM. The note says which one, and why —
/// a config that fails to parse silently falling back to LLVM would make every number this tool prints a lie.
fn style_for(path: &Path) -> (config::Style, Option<String>) {
    let mut directory = path.parent().map(Path::to_path_buf);
    while let Some(current) = directory {
        let candidate = current.join(".clang-format");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return match config::parse(&text, None) {
                Ok(style) => (style, None),
                Err(error) => (
                    config::Style::default_style(),
                    Some(format!("{}: {error}; using the LLVM fallback", candidate.display())),
                ),
            };
        }
        directory = current.parent().map(Path::to_path_buf);
    }
    (config::Style::default_style(), None)
}

/// Every `.c`/`.cc`/`.cpp`/`.h`/`.hpp` at or below `root`.
fn collect(root: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "hh")
        ) {
            files.push(path.to_string_lossy().into_owned());
        }
    }
}

/// Which token indices sit on a `#` directive line. The two streams have the same token sequence, so one map serves
/// both.
fn directive_line_gaps(mine: &tokens::Stream<'_>, theirs: &tokens::Stream<'_>) -> Vec<bool> {
    let mut marks = vec![false; mine.tokens.len()];
    let mut on_directive = false;
    for index in 0..mine.tokens.len() {
        if index > 0 && mine.tokens[index].kind == cpp_parser::CppTokenKind::Hash
            && mine.gaps[index].newlines > 0
        {
            on_directive = true;
        }
        marks[index] = on_directive;
        let ends = mine.gaps.get(index + 1).is_some_and(|gap| gap.newlines > 0)
            || theirs.gaps.get(index + 1).is_some_and(|gap| gap.newlines > 0);
        if ends {
            on_directive = false;
        }
    }
    marks
}

/// The whole line of `text` that contains byte `offset`.
fn line_at(text: &str, offset: usize) -> &str {
    let start = text[..offset.min(text.len())].rfind('\n').map_or(0, |index| index + 1);
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |index| start + index);
    &text[start..end]
}

fn run(clang_format: &str, file: &str) -> Option<String> {
    let output = Command::new(clang_format)
        .args(["-style=file", file])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
