//! **What a run reports** — the findings as text or as one JSON object, and the arithmetic their positions come from.
//!
//! Every number here is a *presentation* of a finding the analysis produced; nothing in this module decides what is
//! wrong with a program. The text form is for a reader (`file:line:col  level  code  message`, grouped by code), the
//! JSON form for a machine (`--json`), and `Fate` carries what became of the files that produced no finding of their
//! own so the summary can say it rather than staying silent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cli::Level;

pub struct Report {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub level: Level,
    pub code: &'static str,
    pub message: String,
}

/// What became of the files that produced no findings of their own.
pub struct Fate {
    pub unreadable: Vec<PathBuf>,
    pub no_reading: Vec<PathBuf>,
    pub withheld: usize,
    pub parse_errors: usize,
    pub checked: usize,
    pub seconds: f64,
}

/// **A byte offset as a 1-based line and column**, in the file's own text.
pub fn position_of(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|newline| offset - newline)
        .unwrap_or(offset + 1);
    (line, column)
}

pub fn report_text(root: &Path, files: &[PathBuf], findings: &[Report], told: &Fate) {
    for finding in findings {
        println!(
            "{}:{}:{}  {}  {}  {}",
            relative(root, &finding.file),
            finding.line,
            finding.column,
            finding.level.text(),
            finding.code,
            finding.message
        );
    }

    // **By code, because that is what a reader acts on**: one rule with forty hits is one decision (fix it or turn it
    // off), and two rules with twenty each are two.
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in findings {
        *by_code.entry(finding.code).or_default() += 1;
    }
    if !by_code.is_empty() {
        println!("\nby code:");
        for (code, count) in by_code {
            println!("  {count:>5}  {code}");
        }
    }

    println!(
        "\n{} file(s) checked of {}, {} finding(s), {} parse error(s), {:.1} s{}",
        told.checked,
        files.len(),
        findings.len(),
        told.parse_errors,
        told.seconds,
        notes(told)
    );
}

pub fn report_json(root: &Path, files: &[PathBuf], findings: &[Report], told: &Fate) {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for finding in findings {
        *by_code.entry(finding.code).or_default() += 1;
    }
    let value = serde_json::json!({
        "root": root.display().to_string(),
        "files_checked": told.checked,
        "files_seen": files.len(),
        "findings": findings.iter().map(|finding| serde_json::json!({
            "file": relative(root, &finding.file),
            "line": finding.line,
            "column": finding.column,
            "severity": finding.level.text(),
            "code": finding.code,
            "message": finding.message,
        })).collect::<Vec<_>>(),
        "by_code": by_code,
        "parse_errors": told.parse_errors,
        "unreadable": told.unreadable.iter().map(|file| relative(root, file)).collect::<Vec<_>>(),
        "no_reading": told.no_reading.iter().map(|file| relative(root, file)).collect::<Vec<_>>(),
        "checks_withheld": told.withheld,
        "seconds": told.seconds,
    });
    println!("{value}");
}

/// The sentence a run adds when something about the files themselves needs saying.
fn notes(told: &Fate) -> String {
    let mut notes = String::new();
    if !told.unreadable.is_empty() {
        notes.push_str(&format!(
            "; {} file(s) could not be read: {}",
            told.unreadable.len(),
            first_few(&told.unreadable)
        ));
    }
    if !told.no_reading.is_empty() {
        notes.push_str(&format!(
            "; {} file(s) had no reading: {}",
            told.no_reading.len(),
            first_few(&told.no_reading)
        ));
    }
    if told.withheld > 0 {
        notes.push_str(&format!(
            "; {} file(s) had their checks withheld while the project was still being read — run again for those",
            told.withheld
        ));
    }
    notes
}

fn first_few(files: &[PathBuf]) -> String {
    let mut shown = files
        .iter()
        .take(3)
        .map(|file| file.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > 3 {
        shown.push_str(", …");
    }
    shown
}

/// **A path as the run names it** — relative to the root, because a check report is read beside the project.
pub fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
}
