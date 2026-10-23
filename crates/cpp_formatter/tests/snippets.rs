//! **The snippet gate, as a Rust test**: every snippet in `tests/snippets/` formats to its captured output.
//!
//! ```text
//! tests/snippets/03_statements.cpp           the input, written to exercise one construct
//! tests/snippets/03_statements.expected.cpp  what this formatter produces for it
//! ```
//!
//! # Why golden files and not a comparison with clang-format
//!
//! `examples/snippet_audit.rs` compares against clang-format and is the tool that *finds* differences. This test is the
//! gate: it runs anywhere, without clang-format, and it fails when the formatter's answer for a snippet changes at all.
//! That is the property worth protecting — a change that is intended shows up as a one-line diff in the `.expected.cpp`
//! file, and a change nobody intended shows up as a failing test.
//!
//! Each snippet is checked twice:
//!
//! ```text
//! format(input)    == expected   the formatting of an unformatted file does not drift
//! format(expected) == expected   the output is a fixed point (what a re-run of the formatter must produce)
//! ```
//!
//! The second property is the one a user feels: formatting a file that is already formatted has to leave it alone.

use std::path::{Path, PathBuf};

/// The snippet directory, from the crate root.
fn directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snippets")
}

/// Every snippet input, in name order (the `.expected.cpp` files are not inputs).
fn snippets() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory())
        .expect("tests/snippets is part of the crate")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            name.ends_with(".cpp") && !name.ends_with(".expected.cpp")
        })
        .collect();
    paths.sort();
    paths
}

fn expected_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|stem| stem.to_str()).expect("a file name");
    directory().join(format!("{stem}.expected.cpp"))
}

#[test]
fn every_snippet_formats_to_its_captured_output() {
    let mut failures = Vec::new();
    for input in snippets() {
        let source = std::fs::read_to_string(&input).expect("a readable snippet");
        let expected_path = expected_path(&input);
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|_| {
            panic!(
                "{} has no captured output — run the formatter on it and save {}",
                input.display(),
                expected_path.display()
            )
        });
        let style = cpp_formatter::config::style_for_file(&input);
        let actual = cpp_formatter::format_file(&input, &source, &style);
        if actual != expected {
            failures.push(format!(
                "{}:\n{}",
                input.file_name().unwrap_or_default().to_string_lossy(),
                first_difference(&expected, &actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} snippet(s) changed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_captured_output_is_a_fixed_point() {
    let mut failures = Vec::new();
    for input in snippets() {
        let expected_path = expected_path(&input);
        let expected = std::fs::read_to_string(&expected_path).expect("a captured output");
        let style = cpp_formatter::config::style_for_file(&expected_path);
        let again = cpp_formatter::format_file(&expected_path, &expected, &style);
        if again != expected {
            failures.push(format!(
                "{}:\n{}",
                input.file_name().unwrap_or_default().to_string_lossy(),
                first_difference(&expected, &again)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} captured output(s) are not stable:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The first differing line, with both texts — enough to see what moved without printing whole files.
fn first_difference(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    for index in 0..expected_lines.len().min(actual_lines.len()) {
        if expected_lines[index] != actual_lines[index] {
            return format!(
                "line {}:\n  expected: {:?}\n  actual:   {:?}",
                index + 1,
                expected_lines[index],
                actual_lines[index]
            );
        }
    }
    format!(
        "line counts differ: expected {} lines, actual {}",
        expected_lines.len(),
        actual_lines.len()
    )
}
