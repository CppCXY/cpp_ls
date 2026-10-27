//! **No message ships with a placeholder still in it.**
//!
//! `rust-i18n`'s `t!` leaves a `%{name}` in its output when the call names no argument for it, and nothing else
//! complains: the string compiles, the translation key is right, and the first reader to see `%{written}` is a user.
//! Measured twice, both in diagnostics the editor shows:
//!
//! ```text
//! cstyle_cast.rs  this is a C-style cast to `%{written}` — the four named casts …
//! use_auto.rs     `%{name}` is declared by its full iterator spelling — write `auto %{name}` …
//! ```
//!
//! The first was found by a real `arglist.c` diagnostics pull on neovim, in the popup text of a C-style cast. The
//! second was found by this scan looking for the first.
//!
//! # Why a source scan
//!
//! The messages are built in dozens of places whose inputs a test would have to construct — a check needs a scope
//! tree, a type, an index. What is wanted here is not "this message renders" but "every message *can* render", and
//! that is a property of the call's text: each `%{name}` in the format string needs a `name = …` argument in the same
//! call. So this reads the crate's sources rather than running them.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `directory`, recursively.
fn rust_files(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// `text` with everything inside a string literal blanked out, so that parentheses in a *message* cannot be mistaken
/// for the call's own. `"` and `\` are kept, and newlines are kept so that offsets still line up.
///
/// **Bytes, not characters**: a blanked character is replaced by as many spaces as its UTF-8 length, because the
/// offsets this returns are used to slice the *original* source (`…` is three bytes, and one space per character
/// put every call after it one byte out of place).
fn without_string_contents(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();

    while let Some(character) = characters.next() {
        match character {
            '"' => {
                masked.push('"');
                while let Some(inner) = characters.next() {
                    match inner {
                        '\\' => {
                            // An escape consumes the next character, whatever it is — `\"` does not close a string.
                            let escaped = characters.next().unwrap_or(' ');
                            masked.push(' ');
                            masked.extend(std::iter::repeat_n(' ', escaped.len_utf8()));
                        }
                        '"' => {
                            masked.push('"');
                            break;
                        }
                        '\n' => masked.push('\n'),
                        _ => masked.extend(std::iter::repeat_n(' ', inner.len_utf8())),
                    }
                }
            }
            _ => masked.push(character),
        }
    }

    masked
}

/// The index just past the `)` that closes the parenthesis at `open`.
fn end_of_call(masked: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, character) in masked[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn every_t_call_fills_its_placeholders() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty(), "the crate's sources are where they are");

    let mut unfilled: Vec<String> = Vec::new();
    let mut calls = 0usize;

    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let masked = without_string_contents(&source);
        let mut from = 0usize;

        while let Some(found) = masked[from..].find("t!(") {
            let at = from + found;
            from = at + 3;
            let open = at + 2; // the `(` of `t!(`

            let Some(close) = end_of_call(&masked, open) else {
                continue;
            };
            // The call's own text, from the *original* source: the masked copy only decided where it ends.
            let call = &source[open..close];
            if !call.contains("%{") {
                continue;
            }
            calls += 1;

            let mut names: Vec<&str> = Vec::new();
            let mut rest = call;
            while let Some(start) = rest.find("%{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find('}') else {
                    break;
                };
                let name = &after[..end];
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|character| character.is_alphanumeric() || character == '_')
                    && !names.contains(&name)
                {
                    names.push(name);
                }
                rest = &after[end + 1..];
            }

            for name in names {
                // **A `name = …` argument**, not just the word: `%{name}` inside the message is not an argument, and
                // the masked text is what the search runs over so a message mentioning `written = x` cannot satisfy it.
                let argument = format!("{name} =");
                if !masked[open..close].contains(&argument) {
                    let line = source[..at].matches('\n').count() + 1;
                    unfilled.push(format!(
                        "{}:{line}: `%{{{name}}}` has no `{name} =` argument",
                        file.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
            }
        }
    }

    assert!(
        calls > 0,
        "the scan found no `t!` call with a placeholder at all, which means it is looking at nothing"
    );
    assert!(
        unfilled.is_empty(),
        "these messages would show a placeholder to the user:\n  {}",
        unfilled.join("\n  ")
    );
}
