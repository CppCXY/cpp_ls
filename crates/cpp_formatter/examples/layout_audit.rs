//! **Whole-file agreement with clang-format, and where the first line diverges.**
//!
//! P0b of `docs/cpp-format.md` §十: the line structure stops being the input's. A formatter that re-indents and
//! re-breaks can be measured the only way that matters — format the same file both ways and compare the text — and
//! the useful part of a mismatch is *where*: an indentation difference, a different line break, a blank line, or a
//! token that moved for another reason.
//!
//! ```text
//! usage: layout_audit <clang-format.exe> <file-or-directory>...
//! ```
//!
//! Two numbers come out. **M1** is files byte-identical to clang-format's output. The classification is of the
//! **first** differing line per file, which is what a person can act on: fixing the most common class raises M1 by
//! every file whose only problem was that class.

use std::path::Path;
use std::process::Command;

use cpp_formatter::config;

#[derive(Default)]
struct Tally {
    files: usize,
    identical: usize,
    skipped: Vec<String>,
    indent: usize,
    lines: usize,
    blank: usize,
    other: usize,
    samples: Vec<String>,
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: layout_audit <clang-format.exe> <file-or-directory>...");
        std::process::exit(2);
    };
    let roots: Vec<String> = arguments.collect();
    if roots.is_empty() {
        eprintln!("layout_audit: no files or directories");
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
    files.sort();

    let mut tally = Tally::default();
    for file in &files {
        let path = Path::new(file);
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let (style, note) = style_for(path);
        if let Some(note) = note {
            eprintln!("layout_audit: {note}");
        }
        let ours = cpp_formatter::format(&source, &style);
        let Some(theirs) = run(&clang_format, file) else {
            eprintln!("layout_audit: clang-format failed on {file}");
            continue;
        };
        tally.files += 1;
        if ours == theirs {
            tally.identical += 1;
            continue;
        }
        // **The token streams must agree** before the difference is a layout one: a file whose *tokens* moved is a
        // spacing or include-sorting problem, which is another step's work and would pollute this measurement.
        let our_stream = cpp_formatter::tokens::stream(&ours);
        let their_stream = cpp_formatter::tokens::stream(&theirs);
        if our_stream.tokens.len() != their_stream.tokens.len()
            || our_stream
                .tokens
                .iter()
                .zip(&their_stream.tokens)
                .any(|(mine, other)| mine.kind != other.kind || mine.text != other.text)
        {
            tally
                .skipped
                .push(format!("{file}: the token streams differ"));
            continue;
        }

        match first_divergence(&ours, &theirs) {
            Divergence::Indent { line, mine, other } => {
                tally.indent += 1;
                if tally.samples.len() < 10 {
                    tally.samples.push(format!(
                        "{file}:{line} indent {mine} -> {other}\n      ours:   {:?}\n      theirs: {:?}",
                        trimmed_at(&ours, line),
                        trimmed_at(&theirs, line),
                    ));
                }
            }
            Divergence::Lines { line, ours_len, theirs_len } => {
                tally.lines += 1;
                if tally.samples.len() < 10 {
                    tally.samples.push(format!(
                        "{file}:{line} line structure (ours has {ours_len} line(s), clang-format {theirs_len})\n      \
                         ours:   {:?}\n      theirs: {:?}",
                        trimmed_at(&ours, line),
                        trimmed_at(&theirs, line),
                    ));
                }
            }
            Divergence::Blank { line } => {
                tally.blank += 1;
                if tally.samples.len() < 10 {
                    tally.samples.push(format!("{file}:{line} blank line count"));
                }
            }
            Divergence::Other { line } => {
                tally.other += 1;
                if tally.samples.len() < 10 {
                    tally.samples.push(format!(
                        "{file}:{line} other\n      ours:   {:?}\n      theirs: {:?}",
                        trimmed_at(&ours, line),
                        trimmed_at(&theirs, line),
                    ));
                }
            }
        }
    }

    println!(
        "whole file: {} of {} identical to clang-format ({:.2}%)",
        tally.identical,
        tally.files,
        tally.identical as f64 * 100.0 / tally.files.max(1) as f64
    );
    let differing = tally.files - tally.identical;
    if differing > 0 {
        println!(
            "first divergence in the {differing} other file(s): indent {}, line structure {}, blank line {}, other {}",
            tally.indent, tally.lines, tally.blank, tally.other
        );
    }
    if !tally.skipped.is_empty() {
        println!("skipped {} file(s):", tally.skipped.len());
        for line in tally.skipped.iter().take(8) {
            println!("  {line}");
        }
    }
    for sample in &tally.samples {
        println!("  {sample}");
    }
}

enum Divergence {
    Indent { line: usize, mine: usize, other: usize },
    Lines { line: usize, ours_len: usize, theirs_len: usize },
    Blank { line: usize },
    Other { line: usize },
}

/// The first line at which the two outputs disagree, classified.
fn first_divergence(ours: &str, theirs: &str) -> Divergence {
    let mine: Vec<&str> = ours.lines().collect();
    let other: Vec<&str> = theirs.lines().collect();

    for line in 0..mine.len().min(other.len()) {
        let (mine_line, other_line) = (mine[line], other[line]);
        if mine_line == other_line {
            continue;
        }
        let mine_body = mine_line.trim_start();
        let other_body = other_line.trim_start();
        if mine_body == other_body {
            return Divergence::Indent {
                line: line + 1,
                mine: mine_line.len() - mine_body.len(),
                other: other_line.len() - other_body.len(),
            };
        }
        if mine_body.is_empty() || other_body.is_empty() {
            return Divergence::Blank { line: line + 1 };
        }
        // A break in one and not the other: our line is a prefix of two of theirs, or the reverse.
        let joined = {
            let mut joined = String::new();
            for next in other.iter().skip(line).take(4) {
                joined.push_str(next.trim());
                if joined.trim_end() == mine_body.trim_end() {
                    break;
                }
            }
            joined
        };
        if joined.trim_end() == mine_body.trim_end() {
            return Divergence::Lines {
                line: line + 1,
                ours_len: 1,
                theirs_len: 2,
            };
        }
        return Divergence::Other { line: line + 1 };
    }
    if mine.len() != other.len() {
        return Divergence::Lines {
            line: mine.len().min(other.len()) + 1,
            ours_len: mine.len(),
            theirs_len: other.len(),
        };
    }
    Divergence::Other { line: 0 }
}

fn trimmed_at(text: &str, line: usize) -> String {
    text.lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .trim()
        .to_string()
}

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
