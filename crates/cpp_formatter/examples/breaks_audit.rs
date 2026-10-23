//! **How closely does the line breaker agree with clang-format, line for line?** — P2's measuring stick.
//!
//! `column_audit` counts lines that are too long, which says "something is wrong" but not *what*. This compares the two
//! formatters on the same input, under the **same style**, token by token: for every token, which line did each one put
//! it on? A token that lands on a different line is a break decision that disagrees, and the ratio is the metric P2 can
//! move.
//!
//! ```text
//! $ breaks_audit <clang-format.exe> <corpus-root> <file>...
//! files: 43 · tokens: 163 597 · tokens on a different line: 61 934 (62.1% agreement)
//! per file: same line count 4 of 43
//! ```
//!
//! # Why the style has to be made equal first
//!
//! The corpus sets `ColumnLimit: 0`, where neither formatter is supposed to break anything: comparing under it would
//! measure nothing. So the audit writes a copy of the corpus `.clang-format` with `ColumnLimit: 80` into a temporary
//! directory, formats copies of the files there with **both** tools under that one style, and compares those outputs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: breaks_audit <clang-format.exe> <corpus-root> <file-or-directory>...");
        std::process::exit(2);
    };
    let Some(corpus) = arguments.next() else {
        eprintln!("breaks_audit: the corpus root (for its `.clang-format`) is required");
        std::process::exit(2);
    };
    let roots: Vec<String> = arguments.collect();

    let Some(style_text) = with_column_limit(&Path::new(&corpus).join(".clang-format"), 80) else {
        eprintln!("breaks_audit: no `.clang-format` under {corpus}");
        std::process::exit(2);
    };
    let style = match cpp_formatter::config::parse(&style_text, None) {
        Ok(style) => style,
        Err(error) => {
            eprintln!("breaks_audit: the style does not parse: {error}");
            std::process::exit(2);
        }
    };

    let scratch = std::env::temp_dir().join(format!("cppfmt-breaks-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    std::fs::write(scratch.join(".clang-format"), &style_text).expect("the scratch style");

    let mut files: Vec<String> = Vec::new();
    for root in &roots {
        let path = PathBuf::from(root);
        if path.is_dir() {
            collect(&path, &mut files);
        } else {
            files.push(root.clone());
        }
    }
    files.sort();

    let mut categories: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut total_tokens = 0usize;
    let mut displaced = 0usize;
    let mut stable_files = 0usize;
    let mut same_line_count = 0usize;
    let mut counted = 0usize;

    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let copy = scratch.join(format!("{counted}.cpp"));
        std::fs::write(&copy, &source).expect("the copy is writable");

        let Some(theirs) = run(&clang_format, &copy) else {
            continue;
        };
        let ours = cpp_formatter::format(&source, &style);
        counted += 1;

        // **The self-consistent half**: clang-format`s own 80-column output fed back through us. The corpus is
        // formatted at `ColumnLimit: 0`, so "we keep its breaks" cannot match a re-flow at 80 — but a text that is
        // *already* 80 columns wide is the fair input, and any token we move on it is a real disagreement.
        let stable = cpp_formatter::format(&theirs, &style);
        if stable == theirs {
            stable_files += 1;
        }
        if ours.lines().count() == theirs.lines().count() {
            same_line_count += 1;
        }

        // The tokens are the same (this is P2, not P3/sorting), so index them by position and compare line numbers.
        let our_tokens = cpp_formatter::tokens::stream(&ours);
        let their_tokens = cpp_formatter::tokens::stream(&theirs);
        if our_tokens.tokens.len() != their_tokens.tokens.len() {
            continue;
        }
        let our_lines = line_numbers(&our_tokens);
        let their_lines = line_numbers(&their_tokens);
        for (index, (mine, other)) in our_lines.iter().zip(their_lines.iter()).enumerate() {
            total_tokens += 1;
            if mine == other {
                continue;
            }
            displaced += 1;
            // **Where the disagreement comes from**: the kind of the token that starts our line and the kind before it,
            // and whether clang-format broke *later* than us (a break we are missing) or *earlier* (one we added).
            let direction = if other > mine { "missing" } else { "extra" };
            let previous = if index == 0 {
                "start".to_string()
            } else {
                format!("{:?}", our_tokens.tokens[index - 1].kind)
            };
            let key = format!("{direction:>7} after {previous}");
            *categories.entry(key).or_insert(0) += 1;
        }
    }

    let agreement = if total_tokens == 0 {
        0.0
    } else {
        100.0 * (total_tokens - displaced) as f64 / total_tokens as f64
    };
    println!(
        "files: {counted} · tokens: {total_tokens} · on a different line: {displaced} ({agreement:.1}% agreement)"
    );
    let mut top: Vec<(&String, &usize)> = categories.iter().collect();
    top.sort_by(|left, right| right.1.cmp(left.1));
    println!("the ten largest disagreements:");
    for (key, count) in top.iter().take(10) {
        println!("  {count:>6}  {key}");
    }
    println!("feeding clang-format`s own 80-column output back through us: {stable_files} of {counted} unchanged");
    println!("files whose line count matches: {same_line_count} of {counted}");
    std::fs::remove_dir_all(&scratch).ok();
}

/// The style text of `path` with `ColumnLimit` replaced (added when the file does not set it).
fn with_column_limit(path: &Path, limit: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = String::with_capacity(text.len());
    let mut seen = false;
    for line in text.lines() {
        if line.trim_start().starts_with("ColumnLimit:") {
            out.push_str(&format!("ColumnLimit: {limit}\n"));
            seen = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !seen {
        out.push_str(&format!("ColumnLimit: {limit}\n"));
    }
    Some(out)
}

/// The line each token is on, counted from its gap's newlines.
fn line_numbers(stream: &cpp_formatter::tokens::Stream<'_>) -> Vec<usize> {
    let mut lines = Vec::with_capacity(stream.tokens.len());
    let mut line = 0usize;
    for index in 0..stream.tokens.len() {
        line += stream.gaps[index].newlines;
        lines.push(line);
    }
    lines
}

fn run(clang_format: &str, file: &Path) -> Option<String> {
    let output = Command::new(clang_format)
        .args(["-style=file", &file.to_string_lossy()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn collect(root: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if name.starts_with(".cppls") || name == "3rd" || name.starts_with("build") {
            continue;
        }
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