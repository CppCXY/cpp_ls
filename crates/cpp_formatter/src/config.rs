//! **`.clang-format`: the YAML subset clang-format itself accepts, and the style it resolves to.**
//!
//! # What is supported, and what happens to the rest
//!
//! Supported: `key: value` (scalars, booleans, integers, quoted strings), **nested maps** (`BraceWrapping:`) and
//! inline sequences (`[a, b]`), `#` comments, `---`-separated documents (each may carry a `Language:`), and
//! `BasedOnStyle: <preset>`.
//!
//! Anything else — anchors, multi-line scalars, flow maps — is a **hard error with the line number**, not a silent
//! ignore. That is the same rule `.cppls.toml` follows: a file a user writes by hand gets a real parser, and the
//! part that is not supported is named rather than skipped.
//!
//! An **unknown key is an error too**, and that is not our invention: clang-format 15 does the same
//! (`error: unknown key 'BogusKey'`, exit code 1 — measured; see `docs/cpp-format.md` §五).
//!
//! # The style is a map, not a struct
//!
//! clang-format 15 has 136 top-level keys and 58 nested sub-keys, and every one of them has a default. A struct
//! would mean writing all of them out and keeping them in sync with the oracle; a flattened map
//! (`"BraceWrapping.AfterClass" -> "false"`) is seeded from [`crate::presets`] and read where a rule needs it.
//! Supporting a key is then a one-line `style.flag("...")` at the place the rule lives, and a key nobody reads is
//! still a key that parses — which is exactly the state the project is measured in (`docs/cpp-format.md` §五).

use std::collections::BTreeMap;

use crate::presets;

/// A resolved style: every key of the chosen preset, overridden by the `.clang-format` that was read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Style {
    values: BTreeMap<String, String>,
}

/// Why a `.clang-format` could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// 1-based line number in the document that failed.
    pub line: usize,
    /// What was being read when it failed.
    pub what: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.what)
    }
}

impl std::error::Error for ConfigError {}

/// The keys that are **not** style options: the loader consumes them.
const LOADER_KEYS: &[&str] = &["BasedOnStyle", "Language", "InheritParentConfig"];

impl Style {
    /// **The style of a preset name** (`LLVM`, `Google`, …, case-insensitive, as clang-format names them).
    pub fn preset(name: &str) -> Option<Style> {
        let canonical = canonical_preset(name)?;

        let mut values: BTreeMap<String, String> = presets::LLVM
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        if let Some(delta) = presets::delta(canonical) {
            for (key, value) in delta {
                values.insert((*key).to_string(), (*value).to_string());
            }
        }
        Some(Style { values })
    }

    /// The `LLVM` style: every key at its default.
    pub fn default_style() -> Style {
        Style::preset("LLVM").expect("LLVM is a preset")
    }

    /// One key's value, as written in `-dump-config`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// An integer key, with clang-format's own default as the fallback.
    pub fn int(&self, key: &str, fallback: i64) -> i64 {
        self.get(key)
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(fallback)
    }

    /// A boolean key (`true`/`false`; clang-format also accepts `Yes`/`No`/`On`/`Off`/`1`/`0`).
    pub fn flag(&self, key: &str, fallback: bool) -> bool {
        match self.get(key).map(str::trim) {
            Some("true") | Some("True") | Some("TRUE") | Some("Yes") | Some("On") | Some("1") => true,
            Some("false") | Some("False") | Some("FALSE") | Some("No") | Some("Off") | Some("0") => false,
            _ => fallback,
        }
    }

    /// A string-valued key (an enum). `fallback` when absent, the raw text when present.
    pub fn string<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.get(key).unwrap_or(fallback)
    }

    /// Is `key` one of the keys this style knows? (Used by the loader to reject typos.)
    pub fn knows(key: &str) -> bool {
        LOADER_KEYS.contains(&key) || presets::LLVM.iter().any(|(known, _)| *known == key)
    }

    /// Every key, as `-dump-config` prints them: sorted, `key: value`, nested keys flattened the way the table is.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.values {
            out.push_str(key);
            out.push(':');
            if !value.is_empty() {
                out.push(' ');
                out.push_str(value);
            }
            out.push('\n');
        }
        out
    }
}

/// The canonical spelling of a preset name, as clang-format accepts it (case-insensitive).
pub fn canonical_preset(name: &str) -> Option<&'static str> {
    const NAMES: &[&str] = &[
        "LLVM",
        "Google",
        "Chromium",
        "Mozilla",
        "WebKit",
        "Microsoft",
        "GNU",
    ];
    NAMES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
}

/// **Read a `.clang-format` document** on top of `base` (`None` = the LLVM default).
///
/// `---`-separated documents are how clang-format scopes a config per language; the first document whose
/// `Language` is absent or `Cpp` is the one that applies.
pub fn parse(text: &str, base: Option<&str>) -> Result<Style, ConfigError> {
    let documents = split_documents(text);

    // Pick the document: the first that names no language or names C++.
    let mut chosen: Option<(usize, &String)> = None;
    for (start_line, document) in &documents {
        let language = document
            .lines()
            .find_map(|line| line.trim().strip_prefix("Language:"))
            .map(|value| value.trim().to_string());
        let applies = match language.as_deref() {
            None | Some("") => true,
            Some("Cpp") => true,
            Some(_) => false,
        };
        if applies {
            chosen = Some((*start_line, document));
            break;
        }
    }

    let Some((start_line, document)) = chosen else {
        return Err(ConfigError {
            line: 1,
            what: "no document applies to C++".to_string(),
        });
    };

    // `BasedOnStyle` first: it decides what the keys below override.
    let mut based_on: Option<String> = None;
    let mut inherited = false;
    for (offset, line) in document.lines().enumerate() {
        let line = strip_comment(line);
        let Some((key, value)) = split_key(&line) else {
            continue;
        };
        let line_number = start_line + offset;
        match key {
            "BasedOnStyle" => based_on = Some(value.trim().to_string()),
            "InheritParentConfig" => inherited = parse_flag(value).ok_or(ConfigError {
                line: line_number,
                what: format!("`InheritParentConfig` needs true or false, got `{value}`"),
            })?,
            _ => {}
        }
    }

    // The base: `BasedOnStyle` in this document, else the caller's fallback, else LLVM.
    let base_name = based_on
        .as_deref()
        .or(base)
        .unwrap_or("LLVM")
        .to_string();
    let mut style = Style::preset(&base_name).ok_or(ConfigError {
        line: start_line,
        what: format!("unknown style `{base_name}`"),
    })?;
    // `InheritParentConfig` is the *loader's* problem; the style records that it was asked for, and
    // `format_file` merges the parent before calling here. (See `docs/cpp-format.md` §五 ④.)
    style.values.insert(
        "InheritParentConfig".to_string(),
        if inherited { "true" } else { "false" }.to_string(),
    );

    apply_document(&mut style, document, start_line)?;
    Ok(style)
}

/// Apply one document's `key: value` lines (and nested maps) to `style`.
fn apply_document(
    style: &mut Style,
    document: &str,
    start_line: usize,
) -> Result<(), ConfigError> {
    // (indent, key) for the enclosing map, so that a nested key becomes `Parent.Child`.
    let mut parents: Vec<(usize, String)> = Vec::new();

    for (offset, raw) in document.lines().enumerate() {
        let line_number = start_line + offset;
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        let (key, value) = split_key(&line).ok_or(ConfigError {
            line: line_number,
            what: format!("not a `key: value` line: `{}`", raw.trim()),
        })?;
        let key = key.trim();
        let value = value.trim();

        while matches!(parents.last(), Some((parent_indent, _)) if *parent_indent >= indent) {
            parents.pop();
        }
        let qualified = match parents.last() {
            Some((_, parent)) => format!("{parent}.{key}"),
            None => key.to_string(),
        };

        if value.is_empty() {
            // A map follows: `BraceWrapping:` with indented children.
            if !Style::knows(&qualified) && !Style::knows(key) {
                return Err(ConfigError {
                    line: line_number,
                    what: format!("unknown key `{key}`"),
                });
            }
            parents.push((indent, qualified));
            continue;
        }

        if LOADER_KEYS.contains(&key) {
            // Already consumed by `parse`, and `Language` is how the document was chosen.
            continue;
        }

        if !Style::knows(&qualified) {
            return Err(ConfigError {
                line: line_number,
                what: format!("unknown key `{qualified}`"),
            });
        }

        style.values.insert(qualified, normalise_value(value));
    }

    Ok(())
}

/// Scalars are stored as they should print back: quoted strings lose their quotes.
fn normalise_value(value: &str) -> String {
    let trimmed = value.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        return trimmed[1..trimmed.len() - 1].to_string();
    }
    trimmed.to_string()
}

fn parse_flag(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "True" | "Yes" | "On" | "1" => Some(true),
        "false" | "False" | "No" | "Off" | "0" => Some(false),
        _ => None,
    }
}

/// Split `key: value` at the first `:` that is not inside quotes.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' | '\'' if quote.is_none() => quote = Some(character),
            '"' | '\'' if quote == Some(character) => quote = None,
            ':' if quote.is_none() => return Some((&line[..index], &line[index + 1..])),
            _ => {}
        }
    }
    None
}

/// Drop a trailing `#` comment — but not one inside a quoted string.
fn strip_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    for (index, character) in line.char_indices() {
        match character {
            '"' | '\'' if quote.is_none() => quote = Some(character),
            '"' | '\'' if quote == Some(character) => quote = None,
            '#' if quote.is_none() => return line[..index].to_string(),
            _ => {}
        }
    }
    line.to_string()
}

/// `---`-separated documents, with the line number each starts on.
fn split_documents(text: &str) -> Vec<(usize, String)> {
    let mut documents: Vec<(usize, String)> = Vec::new();
    let mut current = String::new();
    let mut current_start = 1usize;
    let mut current_has_content = false;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.trim() == "---" {
            if current_has_content {
                documents.push((current_start, std::mem::take(&mut current)));
                current_has_content = false;
            }
            current_start = number + 1;
            continue;
        }
        if !current_has_content && !line.trim().is_empty() {
            current_start = number;
            current_has_content = true;
        }
        current.push_str(line);
        current.push('\n');
    }
    if current_has_content {
        documents.push((current_start, current));
    }
    if documents.is_empty() {
        documents.push((1, text.to_string()));
    }
    documents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_llvm_preset_is_the_base_and_every_other_preset_overrides_it() {
        let llvm = Style::preset("LLVM").unwrap();
        assert_eq!(llvm.int("IndentWidth", 0), 2);
        assert_eq!(llvm.string("BreakBeforeBraces", ""), "Attach");

        // The values are clang-format 15.0.1's own, read from `-dump-config`; Google keeps LLVM's 80 columns and
        // `Attach` braces and differs where the table says it does.
        let google = Style::preset("google").unwrap();
        assert_eq!(google.int("ColumnLimit", 0), 80);
        assert_eq!(google.string("BreakBeforeBraces", ""), "Attach");
        assert_eq!(google.string("PointerAlignment", ""), "Left");
        assert!(google.flag("IndentCaseLabels", false));
        // A key no delta names keeps the LLVM value.
        assert_eq!(google.int("TabWidth", 0), llvm.int("TabWidth", 0));
        // `GNU` is where 79 columns and GNU braces live.
        let gnu = Style::preset("GNU").unwrap();
        assert_eq!(gnu.int("ColumnLimit", 0), 79);
        assert_eq!(gnu.string("BreakBeforeBraces", ""), "GNU");
        let microsoft = Style::preset("Microsoft").unwrap();
        assert_eq!(microsoft.string("BreakBeforeBraces", ""), "Custom");
        assert_eq!(microsoft.int("ColumnLimit", 0), 120);
    }

    #[test]
    fn a_document_overrides_the_preset_and_keeps_the_rest() {
        let style = parse(
            "BasedOnStyle: LLVM\nIndentWidth: 4\nBraceWrapping:\n  AfterClass: true\n",
            None,
        )
        .unwrap();
        assert_eq!(style.int("IndentWidth", 0), 4);
        assert!(style.flag("BraceWrapping.AfterClass", false));
        assert!(!style.flag("BraceWrapping.AfterFunction", true));
        assert_eq!(style.int("ColumnLimit", 0), 80);
    }

    #[test]
    fn an_unknown_key_is_an_error_with_its_line() {
        let error = parse("BasedOnStyle: LLVM\nIndentWidht: 4\n", None).unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.what.contains("IndentWidht"), "{}", error.what);
    }

    #[test]
    fn a_comment_and_a_quoted_colon_do_not_confuse_the_split() {
        let style = parse(
            "IndentWidth: 3 # three\nTabWidth: 8\nIncludeIsMainRegex: 'a:b'\n",
            None,
        )
        .unwrap();
        assert_eq!(style.int("IndentWidth", 0), 3);
        assert_eq!(style.string("IncludeIsMainRegex", ""), "a:b");
    }
}
