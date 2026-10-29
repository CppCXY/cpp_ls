//! `cppfmt` — format C++ with a clang-format-compatible `.clang-format`.
//!
//! ```text
//! cppfmt [options] [files...]
//!   -i, --in-place              rewrite the files instead of printing to stdout
//!   -style=<preset|file>        a preset name (LLVM, Google, …) or `file` to look for `.clang-format`
//!   -fallback-style=<preset>    what `-style=file` falls back to when no `.clang-format` is found (default LLVM)
//!   -dump-config                print the resolved style and exit
//!   -output-replacements-xml  print the edits as clang-format`s XML instead of the formatted text
//! ```
//!

use std::io::Read;
use std::path::{Path, PathBuf};

use cpp_formatter::Style;
use cpp_formatter::config;

fn main() {
    let mut in_place = false;
    let mut style_argument: Option<String> = None;
    let mut fallback: Option<String> = None;
    let mut dump = false;
    let mut replacements_xml = false;
    let mut assume_filename: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();

    for argument in std::env::args().skip(1) {
        if argument == "-i" || argument == "--in-place" {
            in_place = true;
        } else if argument == "-output-replacements-xml" {
            replacements_xml = true;
        } else if argument == "-dump-config" {
            dump = true;
        } else if let Some(value) = argument.strip_prefix("-style=") {
            style_argument = Some(value.to_string());
        } else if let Some(value) = argument.strip_prefix("-fallback-style=") {
            fallback = Some(value.to_string());
        } else if let Some(value) = argument.strip_prefix("-assume-filename=") {
            assume_filename = Some(PathBuf::from(value));
        } else if argument.starts_with('-') {
            eprintln!("cppfmt: unknown option `{argument}`");
            std::process::exit(2);
        } else {
            files.push(PathBuf::from(argument));
        }
    }

    // A style applies to every file in this run; `file` means "look one up next to each file", which is what
    // clang-format does and what an editor expects.
    let fallback_name = fallback.unwrap_or_else(|| "LLVM".to_string());

    if dump {
        // `-dump-config` with a file argument resolves the style the way formatting that file would, which is what
        // makes it a useful diagnostic: the resolved values, not the preset's.
        let style = match resolve(
            &style_argument,
            files.first().map(Path::new),
            &fallback_name,
        ) {
            Ok(style) => style,
            Err(message) => {
                eprintln!("cppfmt: {message}");
                std::process::exit(1);
            }
        };
        print!("{}", style.dump());
        return;
    }

    if files.is_empty() {
        // stdin -> stdout, with the style resolved from `-assume-filename` when it was given.
        let mut source = String::new();
        if let Err(error) = std::io::stdin().read_to_string(&mut source) {
            eprintln!("cppfmt: cannot read stdin: {error}");
            std::process::exit(1);
        }
        let style = match resolve(&style_argument, assume_filename.as_deref(), &fallback_name) {
            Ok(style) => style,
            Err(message) => {
                eprintln!("cppfmt: {message}");
                std::process::exit(1);
            }
        };
        let formatted = cpp_formatter::format(&source, &style);
        if replacements_xml {
            print!(
                "{}",
                cpp_formatter::output::replacements_xml(&source, &formatted, None)
            );
        } else {
            print!("{formatted}");
        }
        return;
    }

    let mut failed = false;
    for file in &files {
        let style = match resolve(&style_argument, Some(file), &fallback_name) {
            Ok(style) => style,
            Err(message) => {
                eprintln!("cppfmt: {message}");
                failed = true;
                continue;
            }
        };
        let Ok(source) = std::fs::read_to_string(file) else {
            eprintln!("cppfmt: cannot read {}", file.display());
            failed = true;
            continue;
        };
        let formatted = cpp_formatter::format_file(Path::new(file), &source, &style);
        if replacements_xml {
            print!(
                "{}",
                cpp_formatter::output::replacements_xml(&source, &formatted, Some(Path::new(file)))
            );
            continue;
        }
        if in_place {
            if formatted != source
                && let Err(error) = std::fs::write(file, &formatted)
            {
                eprintln!("cppfmt: cannot write {}: {error}", file.display());
                failed = true;
            }
        } else {
            print!("{formatted}");
        }
    }

    if failed {
        std::process::exit(1);
    }
}

/// Resolve the style for `file`: an explicit preset, or the nearest `.clang-format` walking up from the file.
fn resolve(
    style_argument: &Option<String>,
    file: Option<&Path>,
    fallback: &str,
) -> Result<Style, String> {
    let Some(argument) = style_argument else {
        return resolve_from_file(file, fallback);
    };
    if argument == "file" {
        return resolve_from_file(file, fallback);
    }
    if argument.starts_with('{') {
        // `-style='{BasedOnStyle: LLVM, IndentWidth: 4}'`: the same document parser, one setting per line.
        let inner = &argument[1..argument.len().saturating_sub(1)];
        let document = inner.replace(',', "\n");
        return config::parse(&document, Some(fallback)).map_err(|error| format!("{error}"));
    }
    match Style::preset(argument) {
        Some(style) => Ok(style),
        None => Err(format!("unknown style `{argument}`")),
    }
}

fn resolve_from_file(file: Option<&Path>, fallback: &str) -> Result<Style, String> {
    let Some(file) = file else {
        // **The project`s fallback is LLVM with a four-space indent** (`config::fallback_style`); an explicit
        // `-fallback-style` other than LLVM is honoured as given.
        return Ok(fallback_or_default(fallback));
    };
    match find_config(file) {
        Some((path, text)) => config::parse(&text, Some(fallback))
            .map_err(|error| format!("{}: {error}", path.display())),
        None => Ok(fallback_or_default(fallback)),
    }
}

/// The style a run falls back to when no `.clang-format` decides: **LLVM with a four-space indent**, which is this
/// project`s convention. Any other `-fallback-style` keeps clang-format`s own meaning.
fn fallback_or_default(fallback: &str) -> Style {
    if fallback.eq_ignore_ascii_case("LLVM") {
        return Style::fallback_style();
    }
    Style::preset(fallback).unwrap_or_else(Style::fallback_style)
}

/// The nearest `.clang-format` at or above `file`.
fn find_config(file: &Path) -> Option<(PathBuf, String)> {
    let mut directory = file.parent().map(Path::to_path_buf);
    // A relative path with no parent (`a.cpp`) starts at the current directory.
    if directory.is_none() {
        directory = std::env::current_dir().ok();
    }
    while let Some(current) = directory {
        let candidate = current.join(".clang-format");
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            return Some((candidate, text));
        }
        directory = current.parent().map(Path::to_path_buf);
    }
    None
}
