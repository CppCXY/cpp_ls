//! **What column does clang-format give a trailing comment?** — a measurement, not a formatter.
//!
//! Run it over a directory of files that `clang-format` has already formatted:
//!
//! ```text
//! $ trailing_probe <dir>
//! groups: 120 · lines: 240
//! delta (column - max code end) by the last character of the code:
//!   ',' 0 × 220, 1 × 12
//!   ';' 1 × 6
//! ```
//!
//! The question it answers is the one the last attempts could not: is the column `max(code end) + N`, and if not, what
//! does the *corpus* say the delta is? A rule that has to be guessed from six-line snippets is what produced three
//! regressions (docs/cpp-format.md §四十四, §五十二, §五十五); this measures the same thing over every trailing comment
//! in the corpus at once.

use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(root) = arguments.next() else {
        eprintln!("usage: trailing_probe <directory-of-formatted-files>");
        std::process::exit(2);
    };
    let mut files: Vec<String> = Vec::new();
    collect(Path::new(&root), &mut files);
    files.sort();

    // (last character of the code) -> (delta -> count)
    let mut table: BTreeMap<char, BTreeMap<usize, usize>> = BTreeMap::new();
    let mut detail: Vec<String> = Vec::new();
    let mut groups = 0usize;
    let mut lines = 0usize;

    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        // One group: consecutive lines that each end in a trailing comment.
        let mut group: Vec<(char, usize, usize)> = Vec::new(); // (last char, code end, comment column)
        let mut flush = |group: &mut Vec<(char, usize, usize)>| {
            if group.is_empty() {
                return;
            }
            groups += 1;
            if detail.len() < 24 {
                let widest = group.iter().map(|(_, code_end, _)| *code_end).max().unwrap_or(0);
                let column = group[0].2;
                detail.push(format!("size {} · max code end {} · column {} · delta {}", group.len(), widest, column, column.saturating_sub(widest)));
            }
            let widest = group.iter().map(|(_, code_end, _)| *code_end).max().unwrap_or(0);
            for (last, _, column) in group.iter() {
                lines += 1;
                let delta = column.saturating_sub(widest);
                *table.entry(*last).or_default().entry(delta).or_default() += 1;
            }
            group.clear();
        };
        for line in text.lines() {
            match trailing_comment(line) {
                Some((code_end, column)) => {
                    let last = line[..code_end].chars().next_back().unwrap_or(' ');
                    group.push((last, code_end, column));
                }
                None => flush(&mut group),
            }
        }
        flush(&mut group);
    }

    println!("groups: {groups} · lines: {lines}");
    // **The groups themselves**, for the cases the table cannot separate.
    println!("groups with their widths:");
    println!("  {}", detail.join("\n  "));

    println!("delta (comment column - max code end of its group) by last character of the code:");
    for (last, deltas) in &table {
        let rendered: Vec<String> = deltas.iter().map(|(delta, count)| format!("{delta} × {count}")).collect();
        println!("  '{last}' {}", rendered.join(", "));
    }
}

/// The code end and the comment column of a line with a trailing comment, outside string and character literals.
fn trailing_comment(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut quote: Option<u8> = None;
    let mut seen_code = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                seen_code = true;
                index += 1;
            }
            b'/' if index + 1 < bytes.len() && (bytes[index + 1] == b'/' || bytes[index + 1] == b'*') => {
                if !seen_code {
                    return None;
                }
                let code_end = line[..index].trim_end().len();
                return Some((code_end, index));
            }
            b' ' | b'\t' | b'\r' => index += 1,
            _ => {
                seen_code = true;
                index += 1;
            }
        }
    }
    None
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