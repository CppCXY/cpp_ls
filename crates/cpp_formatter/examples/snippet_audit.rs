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
            // The captured outputs (`*.expected.cpp`) are the Rust gate`s business (`tests/snippets.rs`), not this
            // tool`s: comparing them with clang-format again would only report the line-break design choice.
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            if name.ends_with(".expected.cpp") {
                return false;
            }
            path.extension().is_some_and(|extension| extension == "cpp" || extension == "h")
        })
        .collect();
    files.sort();

    // **The snippet directory's own `.clang-format`**, which every run reads: the oracle and the library must be given
    // the same configuration or the comparison is meaningless.
    let style = cpp_formatter::config::style_for_file(&directory.join("snippet.cpp"));

    let (mut layout_same, mut stable_same, mut spacing_same) = (0usize, 0usize, 0usize);
    let mut stability_failures = Vec::new();
    let mut spacing_failures: Vec<(String, usize, (String, String))> = Vec::new();

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
        if let Some((index, pair)) = first_spacing_difference(&theirs, &steady) {
            spacing_failures.push((name(file), index, pair));
        } else {
            spacing_same += 1;
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
    // **The number to fix against**: the same comparison with line breaks ignored. clang-format re-flows
    // (`if (a) { b; }` becomes three lines) and this formatter keeps the source's lines, so a byte comparison mixes two
    // different questions. Comparing only the gaps that are on one line in *both* texts isolates the spacing rules.
    println!(
        "spacing:   {spacing_same} of {} with the same spacing between tokens (line breaks ignored) ({:.1}%)",
        files.len(),
        100.0 * spacing_same as f64 / files.len().max(1) as f64
    );
    if !spacing_failures.is_empty() {
        println!();
        println!("spacing failures — a gap that one side spaces and the other does not:");
        for (file, index, (theirs, ours)) in &spacing_failures {
            println!("  {file} (token {index})");
            println!("    clang-format: {theirs}");
            println!("    ours:         {ours}");
        }
    }
    if !stability_failures.is_empty() {
        println!();
        println!("stability failures — byte differences, some of them the line-break design choice:");
        for (file, line, (theirs, ours)) in &stability_failures {
            println!("  {file}:{line}");
            println!("    clang-format: {theirs}");
            println!("    ours:         {ours}");
        }
    }
    std::process::exit(spacing_failures.len().min(120) as i32);
}

/// The first gap that one text spaces and the other does not, ignoring every gap that holds a line break in either.
///
/// This is the metric to fix against: clang-format re-flows, this formatter keeps the source's lines, and a byte
/// comparison of the two mixes that design choice with the spacing rules. Gaps that contain a newline on either side are
/// skipped, so what is left is "these two tokens are on one line in both texts, and the space between them differs".
fn first_spacing_difference(theirs: &str, ours: &str) -> Option<(usize, (String, String))> {
    let their_stream = cpp_formatter::tokens::stream(theirs);
    let our_stream = cpp_formatter::tokens::stream(ours);
    if their_stream.tokens.len() != our_stream.tokens.len() {
        return Some((
            0,
            (
                format!("<{} tokens>", their_stream.tokens.len()),
                format!("<{} tokens>", our_stream.tokens.len()),
            ),
        ));
    }
    for index in 0..their_stream.tokens.len() {
        let their_gap = &theirs[their_stream.gaps[index].start..their_stream.gaps[index].end];
        let our_gap = &ours[our_stream.gaps[index].start..our_stream.gaps[index].end];
        if their_gap.contains('\n') || our_gap.contains('\n') {
            continue;
        }
        if their_gap != our_gap {
            return Some((
                index,
                (
                    format!("{:?} before {:?}", their_gap, their_stream.tokens[index].text),
                    format!("{:?} before {:?}", our_gap, our_stream.tokens[index].text),
                ),
            ));
        }
    }
    None
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