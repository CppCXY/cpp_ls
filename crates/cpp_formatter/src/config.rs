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
//! An **unknown key is an error too**, and that is not our invention: clang-format does the same
//! (`error: unknown key 'BogusKey'`, exit code 1 — measured; see `docs/cpp-format.md` §五).
//!
//! # The style is an array, not a map
//!
//! 188 options, every one with a default, and a formatting rule asks for one of them **hundreds of times per
//! kilobyte** — `space::between` alone consults four or five per gap. A `BTreeMap<String, String>` makes each of
//! those a string comparison *and* a `parse::<i64>()`; that is why the first version of this module was the wrong
//! shape to build the rest of the formatter on.
//!
//! So the store is
//!
//! ```text
//! Option              an enum, `#[repr(usize)]`, one variant per clang-format key ([`crate::options`])
//! Style.values        Vec<Value>, indexed by `option as usize` — one bounds-checked index per query
//! Value               Flag(bool) | Number(i64) | Text(Cow<'static, str>) | Unset
//! ```
//!
//! The **value is parsed once**, when the configuration is read, never at a query. The string form survives in
//! exactly two places: reading a document (`StyleOption::from_key`, a binary search over a sorted table) and printing
//! `-dump-config`. A misspelt key is still an error with its line number, because the loader looks the key up
//! before it stores anything.

use std::borrow::Cow;

use crate::options::StyleOption;
use crate::presets;

/// A resolved style: every option of the chosen preset, overridden by the `.clang-format` that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Style {
    /// `values[option as usize]` — the whole point of the enum.
    values: Vec<Value>,
}

/// One option's value, parsed at load time so that no query parses anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// The option was never set and the preset did not carry it. (Possible only for a key a preset omits.)
    Unset,
    /// `true`/`false`, and the spellings clang-format accepts for them (`Yes`, `On`, …).
    Flag(bool),
    /// An integer, already parsed.
    Number(i64),
    /// An enum-valued option, or a string (`IncludeIsMainRegex`). Borrows the preset's own text when it can.
    Text(Cow<'static, str>),
}

impl Default for Style {
    fn default() -> Self {
        Style::default_style()
    }
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

/// **The style of a file**: the nearest `.clang-format` at or above it, or the default style when there is none.
///
/// This is the rule `clang-format` itself uses, and both entry points need it: the command-line tool and the language
/// server resolve a file's style the same way or a user sees two different answers from one machine.
pub fn style_for_file(file: &std::path::Path) -> Style {
    match find_config(file) {
        Some((_, text)) => parse(&text, None).unwrap_or_else(|_| Style::default_style()),
        None => Style::fallback_style(),
    }
}

/// The nearest `.clang-format` at or above `file`, as the path it was found at and its text.
pub fn find_config(file: &std::path::Path) -> Option<(std::path::PathBuf, String)> {
    let mut directory = file.parent().map(std::path::Path::to_path_buf);
    // A relative path with no parent (`a.cpp`) starts at the current directory.
    if directory.is_none() {
        directory = std::env::current_dir().ok();
    }
    while let Some(current) = directory {
        let candidate = current.join(".clang-format");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return Some((candidate, text));
        }
        directory = current.parent().map(std::path::Path::to_path_buf);
    }
    None
}
impl Style {
    /// **The style of a preset name** (`LLVM`, `Google`, …, case-insensitive, as clang-format names them).
    pub fn preset(name: &str) -> Option<Style> {
        let canonical = canonical_preset(name)?;

        let mut style = Style {
            values: vec![Value::Unset; StyleOption::COUNT],
        };
        for (key, value) in presets::LLVM {
            style.set_from_text(key, value);
        }
        if let Some(delta) = presets::delta(canonical) {
            for (key, value) in delta {
                style.set_from_text(key, value);
            }
        }
        Some(style)
    }

    /// The `LLVM` style: every option at its default.
    pub fn default_style() -> Style {
        Style::preset("LLVM").expect("LLVM is a preset")
    }

    /// **The style used when a file has no `.clang-format`**: LLVM with a four-space indent.
    ///
    /// clang-format`s own fallback is LLVM, whose `IndentWidth` is 2. This project formats C++ at four spaces, so the
    /// fallback differs on that one option and on nothing else: `-style=LLVM` still gives clang-format`s two-space file,
    /// and a `.clang-format` always wins over both.
    pub fn fallback_style() -> Style {
        let mut style = Style::default_style();
        style.set_from_text("IndentWidth", "4");
        style
    }

    /// **One option's value.** The query every formatting rule makes, and it is an index.
    pub fn value(&self, option: StyleOption) -> &Value {
        self.values.get(option as usize).unwrap_or(&Value::Unset)
    }

    /// A boolean option. A number is true when not zero; text is read the way clang-format reads a flag.
    pub fn flag(&self, option: StyleOption) -> bool {
        match self.value(option) {
            Value::Flag(value) => *value,
            Value::Number(value) => *value != 0,
            Value::Text(text) => {
                matches!(text.as_ref(), "true" | "True" | "TRUE" | "Yes" | "On" | "1")
            }
            Value::Unset => false,
        }
    }

    /// An integer option, already parsed — no `parse::<i64>()` at the call site.
    pub fn number(&self, option: StyleOption) -> i64 {
        match self.value(option) {
            Value::Number(value) => *value,
            Value::Flag(value) => *value as i64,
            Value::Text(text) => text.trim().parse().unwrap_or(0),
            Value::Unset => 0,
        }
    }

    /// A string-valued option (an enum, a regex). `""` when the option is not text.
    pub fn text(&self, option: StyleOption) -> &str {
        match self.value(option) {
            Value::Text(text) => text.as_ref(),
            _ => "",
        }
    }

    /// An integer option as a count of spaces or columns — the form most rules want.
    pub fn count(&self, option: StyleOption) -> usize {
        self.number(option).max(0) as usize
    }

    /// **Set an option from the text a document wrote**, parsing it once.
    ///
    /// The kind is decided by the text, not by the key: `-dump-config` prints booleans as `true`/`false` and numbers
    /// as digits, and a document may write either spelling for either kind (`1` for a flag, `"4"` for a count).
    /// `flag`/`number`/`text` each accept the value in any of the three shapes, so the decision is not load-bearing
    /// for correctness — only for how fast the query is.
    pub fn set_from_text(&mut self, key: &str, text: &str) {
        let Some(option) = StyleOption::from_key(key) else {
            return;
        };
        let trimmed = text.trim();
        let value = if trimmed.is_empty() {
            Value::Text(Cow::Borrowed(""))
        } else if let Some(flag) = parse_flag(trimmed) {
            Value::Flag(flag)
        } else if let Ok(number) = trimmed.parse::<i64>() {
            Value::Number(number)
        } else if let Some(borrowed) = borrow_a_preset_value(trimmed) {
            borrowed
        } else {
            Value::Text(Cow::Owned(normalise_value(trimmed)))
        };
        if let Some(slot) = self.values.get_mut(option as usize) {
            *slot = value;
        }
    }

    /// Is `key` one of the keys this style knows? (Used by the loader to reject typos.)
    pub fn knows(key: &str) -> bool {
        LOADER_KEYS.contains(&key)
            || StyleOption::from_key(key).is_some()
            // **A key clang-format still parses but no longer dumps.** Its `-dump-config` is the authority on what a
            // style *has*, not on what a document may *write*: the corpus's `.clang-format` sets
            // `AllowAllConstructorInitializersOnNextLine`, and 22.1.3 formats happily with it while never printing it.
            || presets::PARSED_BUT_NOT_DUMPED.contains(&key)
            // **A struct-valued option is known by its name too.** `-dump-config` prints `BraceWrapping:` followed by
            // its children and never prints a `BraceWrapping` line of its own, so the parent exists only as a prefix;
            // a document writing `BraceWrapping:` and indenting under it is writing the shape the dump writes.
            || StyleOption::ALL.iter().any(|option| {
                let name = option.key();
                name.len() > key.len()
                    && name.starts_with(key)
                    && name.as_bytes()[key.len()] == b'.'
            })
    }

    /// Every option, as `-dump-config` prints them: sorted, `key: value`, nested keys flattened the way the table
    /// is.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for option in StyleOption::ALL {
            let printed = match self.value(option) {
                Value::Flag(true) => "true".to_string(),
                Value::Flag(false) => "false".to_string(),
                Value::Number(value) => value.to_string(),
                Value::Text(text) => text.to_string(),
                Value::Unset => String::new(),
            };
            out.push_str(option.key());
            out.push(':');
            if !printed.is_empty() {
                out.push(' ');
                out.push_str(&printed);
            }
            out.push('\n');
        }
        out
    }
}

/// **A text that is already `'static` is borrowed rather than copied**: every value in the preset tables is.
fn borrow_a_preset_value(text: &str) -> Option<Value> {
    presets::LLVM
        .iter()
        .find(|(_, value)| *value == text)
        .map(|(_, value)| Value::Text(Cow::Borrowed(*value)))
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
    for line in document.lines() {
        let line = strip_comment(line);
        let Some((key, value)) = split_key(&line) else {
            continue;
        };
        if key == "BasedOnStyle" {
            based_on = Some(value.trim().to_string());
        }
    }

    // The base: `BasedOnStyle` in this document, else the caller's fallback, else LLVM.
    let base_name = based_on.as_deref().or(base).unwrap_or("LLVM").to_string();
    let mut style = Style::preset(&base_name).ok_or(ConfigError {
        line: start_line,
        what: format!("unknown style `{base_name}`"),
    })?;

    apply_document(&mut style, document, start_line)?;
    Ok(style)
}

/// Apply one document's `key: value` lines (and nested maps) to `style`.
fn apply_document(style: &mut Style, document: &str, start_line: usize) -> Result<(), ConfigError> {
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

        // **A renamed key is translated, not dropped**: the oracle still reads it, so a style that writes it must
        // resolve the way the oracle resolves it (`successors`).
        if let Some(mapped) = successors(&qualified, value) {
            for (successor_key, successor_value) in mapped {
                style.set_from_text(successor_key, successor_value);
            }
            continue;
        }

        style.set_from_text(&qualified, value);
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
        "true" | "True" | "TRUE" | "Yes" | "On" => Some(true),
        "false" | "False" | "FALSE" | "No" | "Off" => Some(false),
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

/// **A key clang-format still accepts but renamed, and what it means now.**
///
/// The dump is the authority on what a style *has*; a document may still write what the oracle still *reads*. That
/// distinction is measured, not assumed: 22.1.3 formats this corpus with `AllowAllConstructorInitializersOnNextLine`
/// in its `.clang-format` while never printing it — and it applies the old spellings, e.g.
/// `SpacesInParentheses: true` really does put spaces inside every pair of parentheses.
///
/// The mapping is the successor option in 22.1.3's own vocabulary. Where a boolean became an enum, the value is
/// translated rather than stored: `SpacesInParentheses: true` is `SpacesInParens: Custom` with all four sub-options
/// on (measured against 22.1.3: `Custom` + `SpacesInParensOptions.Other` reproduces the old output exactly, and
/// `Always` is not a value of the new key at all).
fn successors(key: &str, value: &str) -> Option<Vec<(&'static str, &'static str)>> {
    let on = matches!(value.trim(), "true" | "True" | "Yes" | "On" | "1");
    Some(match key {
        "SpacesInParentheses" => {
            if on {
                vec![
                    ("SpacesInParens", "Custom"),
                    ("SpacesInParensOptions.Other", "true"),
                    ("SpacesInParensOptions.InConditionalStatements", "true"),
                    ("SpacesInParensOptions.InEmptyParentheses", "true"),
                    ("SpacesInParensOptions.InCStyleCasts", "true"),
                ]
            } else {
                vec![("SpacesInParens", "Never")]
            }
        }
        "SpacesInConditionalStatement"
        | "SpacesInCStyleCastParentheses"
        | "SpaceInEmptyParentheses" => vec![
            ("SpacesInParens", "Custom"),
            (
                match key {
                    "SpacesInConditionalStatement" => {
                        "SpacesInParensOptions.InConditionalStatements"
                    }
                    "SpacesInCStyleCastParentheses" => "SpacesInParensOptions.InCStyleCasts",
                    _ => "SpacesInParensOptions.InEmptyParentheses",
                },
                if on { "true" } else { "false" },
            ),
        ],
        "AlignTrailingComments" => vec![(
            "AlignTrailingComments.Kind",
            if on { "Always" } else { "Never" },
        )],
        "KeepEmptyLinesAtTheStartOfBlocks" => {
            vec![(
                "KeepEmptyLinesAtStartOfBlock",
                if on { "true" } else { "false" },
            )]
        }
        "AlwaysBreakTemplateDeclarations" => {
            vec![("BreakTemplateDeclarations", if on { "Yes" } else { "No" })]
        }
        "AlwaysBreakAfterReturnType" => vec![("BreakAfterReturnType", leak(value.trim()))],
        "BreakBeforeInheritanceComma" => vec![("BreakInheritanceList", leak(value.trim()))],
        "BreakConstructorInitializersBeforeComma" => {
            vec![("BreakConstructorInitializers", leak(value.trim()))]
        }
        "ConstructorInitializerAllOnOneLineOrOnePerLine" => {
            vec![(
                "PackConstructorInitializers",
                if on { "BinPack" } else { "NextLine" },
            )]
        }
        _ => return None,
    })
}

/// A tiny leak for a value that has to become `'static` in the table above. Called once per document line that uses a
/// renamed key — a handful per process, and the alternative is a `String` in the table for no gain.
fn leak(value: &str) -> &'static str {
    match value {
        "Left" => "Left",
        "Right" => "Right",
        "BeforeComma" => "BeforeComma",
        "AfterComma" => "AfterComma",
        "CurrentLine" => "CurrentLine",
        "NextLine" => "NextLine",
        "None" => "None",
        "All" => "All",
        "TopLevel" => "TopLevel",
        "AllDefinitions" => "AllDefinitions",
        "BinPack" => "BinPack",
        _ => "None",
    }
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
        assert_eq!(llvm.number(StyleOption::IndentWidth), 2);
        assert_eq!(llvm.text(StyleOption::BreakBeforeBraces), "Attach");

        // The values are clang-format's own, read from `-dump-config`; Google keeps LLVM's 80 columns and
        // `Attach` braces and differs where the table says it does.
        let google = Style::preset("google").unwrap();
        assert_eq!(google.number(StyleOption::ColumnLimit), 80);
        assert_eq!(google.text(StyleOption::BreakBeforeBraces), "Attach");
        assert_eq!(google.text(StyleOption::PointerAlignment), "Left");
        assert!(google.flag(StyleOption::IndentCaseLabels));
        // An option no delta names keeps the LLVM value.
        assert_eq!(
            google.number(StyleOption::TabWidth),
            llvm.number(StyleOption::TabWidth)
        );
        // `GNU` is where 79 columns and GNU braces live.
        let gnu = Style::preset("GNU").unwrap();
        assert_eq!(gnu.number(StyleOption::ColumnLimit), 79);
        assert_eq!(gnu.text(StyleOption::BreakBeforeBraces), "GNU");
        let microsoft = Style::preset("Microsoft").unwrap();
        assert_eq!(microsoft.text(StyleOption::BreakBeforeBraces), "Custom");
        assert_eq!(microsoft.number(StyleOption::ColumnLimit), 120);
    }

    #[test]
    fn a_document_overrides_the_preset_and_keeps_the_rest() {
        let style = parse(
            "BasedOnStyle: LLVM\nIndentWidth: 4\nBraceWrapping:\n  AfterClass: true\n",
            None,
        )
        .unwrap();
        assert_eq!(style.number(StyleOption::IndentWidth), 4);
        assert!(style.flag(StyleOption::BraceWrappingAfterClass));
        assert!(!style.flag(StyleOption::BraceWrappingAfterFunction));
        assert_eq!(style.number(StyleOption::ColumnLimit), 80);
    }

    #[test]
    fn the_nearest_configuration_wins_and_the_default_is_the_fallback() {
        // A tree with the corpus`s widths in the middle directory: the file below it must pick those up, and a file
        // with no `.clang-format` above it must get the default style.
        let root = std::env::temp_dir().join(format!("cppfmt-style-{}", std::process::id()));
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).expect("the tree is creatable");
        std::fs::write(root.join(".clang-format"), "IndentWidth: 7\n")
            .expect("the config is writable");
        let inside = nested.join("x.cpp");
        std::fs::write(&inside, "int x;\n").expect("the file is writable");

        let style = style_for_file(&inside);
        assert_eq!(
            style.count(crate::options::StyleOption::IndentWidth),
            7,
            "the nearest `.clang-format` wins"
        );

        std::fs::remove_dir_all(&root).ok();
        // The fallback is four spaces (this project`s convention), not clang-format`s two.
        let orphan = std::env::temp_dir().join("cppfmt-style-none/x.cpp");
        assert_eq!(
            style_for_file(&orphan).count(crate::options::StyleOption::IndentWidth),
            Style::fallback_style().count(crate::options::StyleOption::IndentWidth),
            "with no configuration anywhere the default style is the answer"
        );
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
        assert_eq!(style.number(StyleOption::IndentWidth), 3);
        assert_eq!(style.text(StyleOption::IncludeIsMainRegex), "a:b");
    }

    #[test]
    fn a_value_is_parsed_once_and_answered_by_index() {
        // **The shape of the optimisation**: a query is an index into an array, so the value's kind is decided when
        // the document is read and never again. `1` for a flag and `"4"` for a count both work, because the
        // accessors read whichever shape they are given.
        let style = parse("IndentWidth: 4\nIndentCaseLabels: 1\n", None).unwrap();
        assert!(matches!(
            style.value(StyleOption::IndentWidth),
            Value::Number(4)
        ));
        // **`1` for a flag is a number that is true**, which is what clang-format accepts; the accessor reads it
        // whichever shape the document wrote.
        assert!(style.flag(StyleOption::IndentCaseLabels));
        assert_eq!(style.count(StyleOption::IndentWidth), 4);
        assert!(style.flag(StyleOption::IndentCaseLabels));
        // And a query allocates nothing: the text of a preset value is borrowed, not copied.
        assert!(matches!(
            Style::default_style().value(StyleOption::BreakBeforeBraces),
            Value::Text(Cow::Borrowed(_))
        ));
    }

    #[test]
    fn a_renamed_key_is_translated_and_not_dropped() {
        // **Measured against the oracle**, not guessed: 22.1.3 formats `f(1)` as `f( 1 )` when the document says
        // `SpacesInParentheses: true`, and `SpacesInParens: Custom` with the four sub-options on is the spelling
        // that reproduces it (`Always` is not even a value of the new key).
        let style = parse("SpacesInParentheses: true\n", None).unwrap();
        assert_eq!(style.text(StyleOption::SpacesInParens), "Custom");
        assert!(style.flag(StyleOption::SpacesInParensOptionsOther));
        assert!(style.flag(StyleOption::SpacesInParensOptionsInConditionalStatements));

        let off = parse("SpacesInParentheses: false\n", None).unwrap();
        assert_eq!(off.text(StyleOption::SpacesInParens), "Never");

        // A boolean that became an enum.
        let comments = parse("AlignTrailingComments: false\n", None).unwrap();
        assert_eq!(
            comments.text(StyleOption::AlignTrailingCommentsKind),
            "Never"
        );
        let comments = parse("AlignTrailingComments: true\n", None).unwrap();
        assert_eq!(
            comments.text(StyleOption::AlignTrailingCommentsKind),
            "Always"
        );

        // And a renamed boolean only.
        let empty = parse("KeepEmptyLinesAtTheStartOfBlocks: true\n", None).unwrap();
        assert!(empty.flag(StyleOption::KeepEmptyLinesAtStartOfBlock));
    }

    #[test]
    fn the_dump_prints_every_option_sorted() {
        let dumped = Style::default_style().dump();
        let keys: Vec<&str> = dumped
            .lines()
            .map(|line| line.split(':').next().unwrap_or(""))
            .collect();
        assert_eq!(keys.len(), StyleOption::COUNT);
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
        assert!(dumped.contains("IndentWidth: 2\n"), "{dumped}");
    }
}
