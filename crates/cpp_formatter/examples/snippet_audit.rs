//! **The snippet audit**: our output against clang-format's on a directory of small files.
//!
//! ```text
//! cargo run --release -p cpp_formatter --example snippet_audit -- <clang-format> <directory>
//! ```
//!
//! # What it measures, and why it is not `layout_audit`
//!
//! `layout_audit` uses a real project formatted at `ColumnLimit: 0`, which is the oracle for the *corpus* but a narrow
//! test: that project has a particular shape (a recursive-descent parser), and it is already laid out the way its
//! configuration wants. This audit formats a directory of snippets written to exercise one construct each, and it
//! compares **two** things:
//!
//! ```text
//! layout    our output against clang-format's, byte for byte, on the snippet as written
//! stability clang-format's own output fed back through us — a text that is already formatted must come back unchanged
//! ```
//!
//! The stability half is what found the four spacing bugs the corpus could not see: `static_assert (x)`,
//! `operator = (`, `auto f =[&]` and `(void) i`. A snippet that fails `layout` may be a design difference (we keep the
//! source's line breaks, clang-format re-flows); a snippet that fails `stability` is always our bug, because the input
//! is already what the oracle considers correct.
//!
//! The exit code is the number of snippets that fail `stability`, so the command is usable as a gate.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: snippet_audit <clang-format> <directory>");
        std::process::exit(2);
    };
    let Some(directory) = arguments.next() else {
        eprintln!("usage: snippet_audit <clang-format> <directory>");
        std::process::exit(2);
    };
    let directory = PathBuf::from(directory);

    let mut files: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "cpp" || extension == "h")
        })
        .collect();
    files.sort();

    // **The snippet directory's own `.clang-format`**, which every run reads: the oracle and the library must be given
    // the same configuration or the comparison is meaningless.
    let style = cpp_formatter::config::style_for_file(&directory.join("snippet.cpp"));

    let (mut layout_same, mut stable_same) = (0usize, 0usize);
    let mut stability_failures = Vec::new();

    for file in &files {
        let source = std::fs::read_to_string(file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        let theirs = match clang_format_output(&clang_format, file) {
            Some(text) => text,
            None => {
                println!("{:<28} skipped (clang-format failed)", name(file));
                continue;
            }
        };

        let ours = cpp_formatter::format_file(file, &source, &style);
        if ours == theirs {
            layout_same += 1;
        } else {
            let (theirs_line, ours_line) = first_difference(&theirs, &ours);
            println!("{:<28} layout differs at {}", name(file), theirs_line);
            println!("    clang-format: {}", ours_line.0);
            println!("    ours:         {}", ours_line.1);
            let _ = ours_line;
        }

        // **The self-consistent half**: the input is already what clang-format produces, so any difference is ours.
        let steady = cpp_formatter::format_file(file, &theirs, &style);
        if steady == theirs {
            stable_same += 1;
        } else {
            let (line, texts) = first_difference(&theirs, &steady);
            stability_failures.push((name(file), line, texts));
        }
    }

    println!();
    println!(
        "layout:    {layout_same} of {} identical to clang-format ({:.1}%)",
        files.len(),
        100.0 * layout_same as f64 / files.len().max(1) as f64
    );
    println!(
        "stability: {stable_same} of {} unchanged when clang-format's own output is fed back ({:.1}%)",
        files.len(),
        100.0 * stable_same as f64 / files.len().max(1) as f64
    );
    if !stability_failures.is_empty() {
        println!();
        println!("stability failures — each one is our bug, not a design difference:");
        for (file, line, (theirs, ours)) in &stability_failures {
            println!("  {file}:{line}");
            println!("    clang-format: {theirs}");
            println!("    ours:         {ours}");
        }
    }
    std::process::exit(stability_failures.len().min(120) as i32);
}

fn name(path: &PathBuf) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn clang_format_output(clang_format: &str, file: &PathBuf) -> Option<String> {
    let output = Command::new(clang_format)
        .args(["-style=file", &file.display().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"))
}

/// The first line where the two texts differ, with both lines — the whole point of the audit is the pair, not a count.
fn first_difference(theirs: &str, ours: &str) -> (usize, (String, String)) {
    let theirs_lines: Vec<&str> = theirs.lines().collect();
    let ours_lines: Vec<&str> = ours.lines().collect();
    for index in 0..theirs_lines.len().min(ours_lines.len()) {
        if theirs_lines[index] != ours_lines[index] {
            return (
                index + 1,
                (theirs_lines[index].to_string(), ours_lines[index].to_string()),
            );
        }
    }
    (
        theirs_lines.len().min(ours_lines.len()),
        (
            format!("<{} lines>", theirs_lines.len()),
            format!("<{} lines>", ours_lines.len()),
        ),
    )
}
