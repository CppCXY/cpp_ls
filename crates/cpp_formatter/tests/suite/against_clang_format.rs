//! **Our formatter against the one Visual Studio ships** — the differential, as a measurement.
//!
//! ```text
//! $env:CPPLS_CLANG_FORMAT = "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\Llvm\x64\bin\clang-format.exe"
//! $env:CPPLS_FMT_CORPUS  = "E:\EmmyLuaCodeStyle\CodeFormatCore\src"
//! $env:CPPLS_FMT_STYLE   = "LLVM"            # or Microsoft, Google, or an inline YAML object
//! cargo test --release -p cpp_formatter --test suite against_clang_format -- --ignored --nocapture
//! ```
//!
//! The question this answers is not "do the two agree on a fixture" — `tests/cases_*.rs` already holds the shapes
//! this crate claims — it is **how much of a real project does a reader get the same text from**, and where the
//! remaining differences start. So the corpus is a directory of files nobody wrote for this test, both formatters are
//! run as a user runs them (`-style=…`, over stdout), and the report is per file and per line.
//!
//! # What is compared
//!
//! The text, after `\r\n` is read as `\n` — a client formatting a buffer gets the same answer either way, and the
//! corpus is a Windows checkout. Nothing else is normalised: a trailing space and a blank line are exactly what a
//! reader sees, and hiding them would hide the defect this exists to find.
//!
//! # Why it is `#[ignore]`d
//!
//! It needs a binary this repository does not build (clang-format) and a corpus that is not checked in. Without
//! `CPPLS_CLANG_FORMAT` the test says so and returns, so a workspace run on a machine with neither stays green.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What the two formatters produced for one file.
struct Compared {
    path: PathBuf,
    /// The first line whose text differs, as `(number, theirs, ours)`.
    first: Option<(usize, String, String)>,
    differing_lines: usize,
    lines: usize,
}

fn corpus() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_FMT_CORPUS")?;
    let root = PathBuf::from(named);
    root.is_dir().then_some(root)
}

fn clang_format() -> Option<PathBuf> {
    let named = PathBuf::from(std::env::var_os("CPPLS_CLANG_FORMAT")?);
    named.is_file().then_some(named)
}

fn style() -> String {
    std::env::var("CPPLS_FMT_STYLE").unwrap_or_else(|_| "LLVM".to_string())
}

/// The corpus's source files, in a stable order, bounded by `CPPLS_FMT_LIMIT` (default 200).
fn sources(root: &Path) -> Vec<PathBuf> {
    let limit: usize = std::env::var("CPPLS_FMT_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let source = matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "ipp")
            );
            if !source {
                continue;
            }
            // **A file with a syntax error is not the formatter's question** — `clang-format` answers one with its
            // own recovery, and comparing two recoveries compares parsers. The size bound is the other half: a
            // generated file of several megabytes takes a minute and says nothing about editing.
            let small = std::fs::metadata(&path)
                .map(|metadata| metadata.len() < 512 * 1024)
                .unwrap_or(false);
            if source && small {
                found.push(path);
            }
        }
    }
    found.sort();
    found.truncate(limit);
    found
}

fn format_with(program: &Path, style: &str, file: &Path) -> Option<String> {
    let output = Command::new(program)
        .arg(format!("-style={style}"))
        .arg(file)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    Some(text)
}

/// **Format text through a program's stdin** — for the second measurement below, which feeds clang-format's own
/// output back to `cppfmt`: a defect in the two formatters' line breaks and a defect in the spacing between tokens
/// are different questions, and only the second one is answerable on text that is already laid out the way
/// clang-format lays it out.
/// Format `text` with `program`, **telling it which file the text belongs to**: `clang-format` promotes a file's
/// own header out of its category (`IncludeIsMainRegex`), so a comparison that pipes stdin without a name loses that
/// promotion on one side only — measured as a two-line include swap that was the harness's, not the formatter's.
fn format_text_with(
    program: &Path,
    style: &str,
    text: &str,
    name: Option<&Path>,
) -> Option<String> {
    use std::io::Write;
    let mut child = Command::new(program);
    child.arg(format!("-style={style}"));
    if let Some(name) = name {
        child.arg(format!("-assume-filename={}", name.display()));
    }
    let mut child = child
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(text.as_bytes())
        .ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"))
}

fn compare(path: &Path, theirs: &str, ours: &str) -> Compared {
    let their_lines: Vec<&str> = theirs.lines().collect();
    let our_lines: Vec<&str> = ours.lines().collect();
    let (n, m) = (their_lines.len(), our_lines.len());
    // **The diff is a longest-common-subsequence one, not a positional zip.** A formatter that breaks a line in two
    // shifts every line after it, and a positional count calls all of them different: the number then measures line
    // offsets rather than the formatter's mistakes. `differing_lines` is the number of lines that change when one
    // text is turned into the other — deleted plus inserted.
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if their_lines[i] == our_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let common = table[0][0] as usize;
    let differing = (n - common) + (m - common);
    let mut first = None;
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if their_lines[i] == our_lines[j] {
            i += 1;
            j += 1;
        } else {
            first = Some((i + 1, their_lines[i].to_string(), our_lines[j].to_string()));
            break;
        }
    }
    if first.is_none() && n != m {
        let at = i.min(n).min(m);
        first = Some((
            at + 1,
            their_lines
                .get(at)
                .copied()
                .unwrap_or("<end of file>")
                .to_string(),
            our_lines
                .get(at)
                .copied()
                .unwrap_or("<end of file>")
                .to_string(),
        ));
    }
    Compared {
        path: path.to_path_buf(),
        first,
        differing_lines: differing,
        lines: n.max(m),
    }
}

#[test]
#[ignore = "a probe: needs clang-format and a corpus, through CPPLS_CLANG_FORMAT / CPPLS_FMT_CORPUS"]
fn the_diff_against_clang_format() {
    let Some(clang) = clang_format() else {
        eprintln!("skipped: set CPPLS_CLANG_FORMAT to the clang-format to compare against");
        return;
    };
    let Some(root) = corpus() else {
        eprintln!("skipped: set CPPLS_FMT_CORPUS to a directory of sources");
        return;
    };
    let style = style();
    let ours = PathBuf::from(env!("CARGO_BIN_EXE_cppfmt"));

    // **Which oracle this run is against**, printed with the numbers: the difference between clang-format 15 and 23 on
    // the Google style alone changed the identical-file count by two and the spacing-only count by seven, so a
    // baseline that does not name its oracle is a number about nothing.
    let version = Command::new(&clang)
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown clang-format".to_string());
    println!("[diff] oracle: {version}");

    let files = sources(&root);
    let mut matched = 0usize;
    let mut spacing_matched = 0usize;
    let mut spacing_differing = 0usize;
    let mut failed = 0usize;
    let mut differing_lines = 0usize;
    let mut total_lines = 0usize;
    let mut worst: Vec<Compared> = Vec::new();

    for file in &files {
        let (Some(theirs), Some(mine)) = (
            format_with(&clang, &style, file),
            format_with(&ours, &style, file),
        ) else {
            failed += 1;
            println!(
                "[diff] {} could not be formatted by one of the two",
                file.display()
            );
            continue;
        };
        // **The second measurement: our spacing on text clang-format already laid out.** A file that differs here
        // differs in a *token-level* decision (a space, an indent, a comment), and a file that matches here but not
        // above differs in where the lines go.
        match format_text_with(&ours, &style, &theirs, Some(file)) {
            Some(on_theirs) if on_theirs == theirs => spacing_matched += 1,
            Some(on_theirs) => {
                spacing_differing += compare(file, &theirs, &on_theirs).differing_lines;
            }
            None => {}
        }
        if theirs == mine {
            matched += 1;
            total_lines += theirs.lines().count();
            continue;
        }
        let compared = compare(file, &theirs, &mine);
        total_lines += compared.lines;
        differing_lines += compared.differing_lines;
        worst.push(compared);
    }

    worst.sort_by_key(|compared| std::cmp::Reverse(compared.differing_lines));
    for compared in worst.iter().take(20) {
        match &compared.first {
            Some((line, theirs, ours)) => println!(
                "[diff] {} — {} of {} line(s) differ; first at {line}:\n  clang: {theirs:?}\n  ours:  {ours:?}",
                compared.path.display(),
                compared.differing_lines,
                compared.lines
            ),
            None => println!(
                "[diff] {} — {} line(s) differ (no differing text: a line-count difference)",
                compared.path.display(),
                compared.differing_lines
            ),
        }
    }
    println!(
        "[diff] style {style}: {matched} of {} file(s) identical, {failed} could not be formatted, \
         {differing_lines} of {total_lines} line(s) differ",
        files.len()
    );
    println!(
        "[diff] style {style}: {spacing_matched} of {} file(s) match on text clang-format already laid out \
         ({spacing_differing} differing line(s) from spacing alone)",
        files.len()
    );
}
