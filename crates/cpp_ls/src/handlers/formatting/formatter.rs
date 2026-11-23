//! **Which formatter a project runs, and how a file reaches it.**
//!
//! `[format] formatter` names one of two things: this repository's own `cpp_formatter`, or a program — `clang-format`
//! by default, by name or by path. The first is a library call; the second is a child process. Everything above this
//! module — the three LSP methods, the edit diff, the cursor-preserving answer — is written against one shape, so the
//! two paths have to agree on what they return: **the formatted text, or a reason it is not there**.
//!
//! # Why `clang-format` is the default, and why a missing one is a warning rather than an error
//!
//! `cpp_formatter` does not yet reflow lines the way clang-format does (see `crates/cpp_formatter/tests/suite/against_clang_format.rs`: on the
//! corpus's own style it matches 27 of 29 files on spacing, but a file whose lines clang-format *moves* is a different
//! file). A project that has clang-format installed therefore gets a better answer from it, and that is what the
//! default asks for. A project that does not has two honest choices, and this module takes the one that keeps
//! formatting working: **fall back to the built-in formatter and say so once**, because a format-on-save that
//! silently stops working is worse than a formatter that reflows less.
//!
//! # The hover's macro expansion is *not* this
//!
//! Hovering a macro expands its body and lays it out, and that layout is [`cpp_formatter::reflow`] with
//! [`cpp_formatter::Style::fallback_style`] — see `Session::the_expansion_of_a_macro` in `cpp_code_analysis`. It stays
//! on the built-in formatter whatever `[format] formatter` says: the hover is a *card* whose text has to be produced
//! inside the server without spawning anything, and a card laid out by an external program would be a request per
//! hover. The two answers are deliberately decided in different places, and this note is the reason.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use lsp_types::Range;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// **The names `[format] formatter` may use for the built-in formatter.** The crate is `cpp_formatter`, the command
/// line tool is `cppfmt`, and both spellings have been written by hand in a configuration file; accepting all three
/// costs a comparison and removes a class of "why is it running clang-format" question.
const BUILT_IN_NAMES: [&str; 3] = ["cpp_formatter", "cpp_format", "cppfmt"];

/// **What `[format] formatter` named.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formatter {
    /// This repository's own formatter: a library call, no child process.
    Builtin,
    /// A program to run, as written: `clang-format` (the default), `clang-format-18`, or a path to one.
    Program(String),
}

impl Formatter {
    /// **Read the configured value.** Anything that is not one of the built-in names is a program, which is the one
    /// rule the configuration promises: a bare name is looked up in `PATH`, a path is run as written, and neither is
    /// checked here (whether a program exists is a question about the machine, answered where it is run).
    pub fn of(configured: &str) -> Self {
        let written = configured.trim();
        if BUILT_IN_NAMES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(written))
        {
            return Formatter::Builtin;
        }
        if written.is_empty() {
            return Formatter::Program(cpp_code_analysis::DEFAULT_FORMATTER.to_string());
        }
        Formatter::Program(written.to_string())
    }
}

/// **Why a program did not format the file.** Split into the three answers that need different words: a missing
/// program is a configuration problem, a refusal is a report about the file, and "not runnable at all" is neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The program is not on this machine — `PATH` has no such name, or the path does not exist.
    NotFound,
    /// It ran and exited non-zero, with whatever it wrote to stderr.
    Refused { status: Option<i32>, stderr: String },
    /// It could not be started at all (a directory, a permission, a broken image).
    Unusable(String),
}

impl Failure {
    /// One sentence for the log, and for the `showMessage` a missing program gets.
    pub fn said(&self, program: &str) -> String {
        match self {
            Failure::NotFound => format!("{program} was not found"),
            Failure::Refused { status, stderr } => match status {
                Some(status) => format!(
                    "{program} exited with status {status}: {}",
                    first_line(stderr)
                ),
                None => format!("{program} was killed: {}", first_line(stderr)),
            },
            Failure::Unusable(reason) => format!("{program} could not be run: {reason}"),
        }
    }
}

/// The first non-empty line of a program's stderr — the rest is a stack of its own, and a card is not the place.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no message")
        .to_string()
}

/// **The command line clang-format documents for a filter**, which is what this is: the text goes in on stdin and the
/// formatted text comes out on stdout.
///
/// `--assume-filename` is what makes the style lookup work at all: with no file argument clang-format has no path to
/// search `.clang-format` from, and the file's own name is also what its main-header rule keys on (`a.cpp`'s own
/// `a.h` first). `--style=file` is the default and is passed anyway: a project's `.clang-format` with
/// `BasedOnStyle` is the whole point, and a default that changes would change this behaviour quietly.
///
/// `lines` is `(first, last)`, **1-based and inclusive**, which is how `--lines` counts. It is how a range format
/// keeps its promise without formatting the whole file and discarding most of it: clang-format lays out the lines that
/// contain the range and leaves the rest as it was.
pub fn clang_format_arguments(path: &Path, lines: Option<(u32, u32)>) -> Vec<String> {
    let mut arguments = vec![
        format!("--assume-filename={}", path.display()),
        "--style=file".to_string(),
    ];
    if let Some((first, last)) = lines {
        arguments.push(format!("--lines={first}:{last}"));
    }
    arguments
}

/// **The lines an LSP `Range` covers, as `clang-format --lines` counts them.**
///
/// `Range.end` is exclusive, and a range that stops at column 0 does not include that line — that is what makes
/// "select two lines and format" format two lines and not three.
pub fn lines_of(range: Range) -> (u32, u32) {
    let first = range.start.line + 1;
    let last = if range.end.character == 0 {
        range.end.line.max(range.start.line)
    } else {
        range.end.line + 1
    };
    (first, last.max(first))
}

/// **Run `program` as clang-format over `text`**, and answer its stdout.
///
/// The text is written on stdin in a task of its own, so a program that writes before it has read everything cannot
/// deadlock against this one; `kill_on_drop` is what makes a cancelled request take the child with it.
pub async fn format_with_clang_format(
    program: &str,
    path: &Path,
    text: &str,
    lines: Option<(u32, u32)>,
    cancel_token: &CancellationToken,
) -> Result<String, Failure> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(clang_format_arguments(path, lines))
        // **The file's own directory, because that is where clang-format starts looking for `.clang-format`** — the
        // assumed filename decides it too, and the two agreeing is what a project with a per-directory style needs.
        .current_dir(path.parent().unwrap_or_else(|| Path::new(".")))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Failure::NotFound);
        }
        Err(error) => return Err(Failure::Unusable(error.to_string())),
    };

    let mut stdin = child.stdin.take().expect("stdin is piped");
    let owned = text.to_string();
    let writer = tokio::spawn(async move {
        // A program that exits without reading its stdin closes the pipe, and that is not this side's failure.
        let _ = stdin.write_all(owned.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });

    let output = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => return Err(Failure::Unusable("the request was cancelled".to_string())),
        output = child.wait_with_output() => output,
    };
    let _ = writer.await;

    let output = match output {
        Ok(output) => output,
        Err(error) => return Err(Failure::Unusable(error.to_string())),
    };
    if !output.status.success() {
        return Err(Failure::Refused {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    // **Bytes, not `from_utf8_lossy`**: clang-format passes a UTF-8 file through unchanged, and a lossy conversion
    // would replace a byte it did not expect with `U+FFFD` — an edit that corrupts the file rather than an error.
    String::from_utf8(output.stdout).map_err(|error| Failure::Unusable(error.to_string()))
}

/// **Take only the requested lines from a formatter's answer to a range request.**
///
/// `clang-format --lines` is documented as "format only a given range of lines", and it is not quite that: measured
/// with 19.1.7 on `int  x = 1;\n   int y = 2;\n`, `--lines=1:1` answers **both** lines reformatted — the neighbour of
/// the range was reindented too. A range format that edits a line outside the selection is the one thing this server
/// promises not to do (the edit list is what a client applies verbatim, and the cursor may be outside it), so the
/// answer is clamped to the lines that were asked for.
///
/// `first` and `last` are **1-based and inclusive**, as `--lines` counts them. A formatter that changed the *number*
/// of lines is not clamped: its line indices no longer describe the original, and splicing by index would produce
/// text from two different layouts. That answer is returned whole — a reflowing formatter has moved the lines the
/// reader selected, and there is nothing honest to keep.
pub fn keep_only_the_lines(first: u32, last: u32, original: &str, formatted: &str) -> String {
    let before = first.saturating_sub(1) as usize;
    let after = last.max(first) as usize;
    let original_lines: Vec<&str> = original.split_inclusive('\n').collect();
    let formatted_lines: Vec<&str> = formatted.split_inclusive('\n').collect();
    if before >= original_lines.len() || formatted_lines.len() != original_lines.len() {
        return formatted.to_string();
    }
    let mut out = String::with_capacity(original.len());
    for line in &original_lines[..before] {
        out.push_str(line);
    }
    for line in formatted_lines
        .iter()
        .skip(before)
        .take(after.saturating_sub(before))
    {
        out.push_str(line);
    }
    for line in original_lines.iter().skip(after) {
        out.push_str(line);
    }
    out
}

/// **Is `program` something this machine can run?**
///
/// A bare name is looked for on `PATH` (with `PATHEXT` on Windows, so `clang-format` finds `clang-format.exe`); a
/// path — absolute, or anything with a separator in it — is looked at directly. This is the question the *startup*
/// warning asks, before any formatting has been requested; a request finds out by spawning, which is the answer that
/// matters and costs nothing extra.
pub fn is_available(program: &str) -> bool {
    let program = program.trim();
    if program.is_empty() {
        return false;
    }
    let path = Path::new(program);
    if path.is_absolute() || program.contains('/') || program.contains('\\') {
        return path.is_file();
    }
    let Some(directories) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&directories).any(|directory| {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return true;
        }
        // On Windows a bare name is completed by `PATHEXT` when a process is spawned, so `clang-format` is
        // `clang-format.exe` on disk. The others (`cmd`, `bat`) are not formatters and are not looked for.
        if cfg!(windows) && candidate.extension().is_none() {
            return candidate.with_extension("exe").is_file();
        }
        false
    })
}

/// **The programs this process has already warned about.**
///
/// A missing `clang-format` is one message per session, not one per save: format-on-save would otherwise open a
/// notification every time the user presses Ctrl+S. The key is the program's name, so two projects that name two
/// different missing programs each get their own message.
static WARNED: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// **Record that this program's problem is about to be reported**, and answer whether this call is the one that must
/// speak. `true` once per program name.
pub fn warn_once(program: &str) -> bool {
    match WARNED.lock() {
        Ok(mut warned) => warned.insert(program.to_string()),
        // A poisoned lock is a panic in another thread; the warning is not worth anything else.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_formatter_is_named_by_its_three_spellings() {
        assert_eq!(Formatter::of("cpp_formatter"), Formatter::Builtin);
        assert_eq!(Formatter::of("  cpp_format "), Formatter::Builtin);
        assert_eq!(Formatter::of("CPPFMT"), Formatter::Builtin);
    }

    #[test]
    fn anything_else_is_a_program() {
        assert_eq!(
            Formatter::of("clang-format"),
            Formatter::Program("clang-format".to_string())
        );
        assert_eq!(
            Formatter::of("D:/LLVM/bin/clang-format.exe"),
            Formatter::Program("D:/LLVM/bin/clang-format.exe".to_string())
        );
        // An empty value is the default, and the default is clang-format — not the built-in one.
        assert_eq!(
            Formatter::of("   "),
            Formatter::Program(cpp_code_analysis::DEFAULT_FORMATTER.to_string())
        );
    }

    #[test]
    fn the_command_line_is_the_one_clang_format_documents() {
        let arguments = clang_format_arguments(Path::new("C:/p/a.cpp"), None);
        assert_eq!(
            arguments,
            vec![
                "--assume-filename=C:/p/a.cpp".to_string(),
                "--style=file".to_string()
            ]
        );
        let arguments = clang_format_arguments(Path::new("C:/p/a.cpp"), Some((3, 7)));
        assert_eq!(arguments.last().unwrap(), "--lines=3:7");
    }

    #[test]
    fn a_range_counts_lines_the_way_the_editor_shows_them() {
        // Lines 2 and 3, selected from the start of 2 to the start of 4.
        let range = Range {
            start: lsp_types::Position {
                line: 2,
                character: 0,
            },
            end: lsp_types::Position {
                line: 4,
                character: 0,
            },
        };
        assert_eq!(
            lines_of(range),
            (3, 4),
            "1-based, and the end's line is not included"
        );

        // A range that stops mid-line includes that line.
        let range = Range {
            start: lsp_types::Position {
                line: 0,
                character: 2,
            },
            end: lsp_types::Position {
                line: 1,
                character: 5,
            },
        };
        assert_eq!(lines_of(range), (1, 2));

        // An empty range is one line, not zero.
        let empty = Range {
            start: lsp_types::Position {
                line: 5,
                character: 3,
            },
            end: lsp_types::Position {
                line: 5,
                character: 3,
            },
        };
        assert_eq!(lines_of(empty), (6, 6));
    }

    #[test]
    fn a_program_that_is_not_there_is_not_available() {
        assert!(!is_available("a-formatter-that-does-not-exist-anywhere"));
        assert!(!is_available(""));
        assert!(!is_available("C:/nowhere/at/all/clang-format.exe"));
    }

    #[test]
    fn a_program_that_is_there_is_available() {
        // The test binary itself is a file that exists, which is all this question asks.
        let itself = std::env::current_exe().expect("the test binary has a path");
        assert!(is_available(&itself.display().to_string()));
        #[cfg(windows)]
        assert!(
            is_available("cmd.exe") || is_available("cmd"),
            "PATH has a shell"
        );
    }

    /// **`--lines` does not keep to its lines, and this is what does.** Measured with clang-format 19.1.7 on
    /// `int  x = 1;\n   int y = 2;\n`: `--lines=1:1` answers both lines reformatted, so the answer has to be clamped
    /// to the lines the client asked for.
    #[test]
    fn only_the_requested_lines_are_taken_from_a_range_answer() {
        let original = "int  x = 1;\n   int y = 2;\n";
        let answer = "int x = 1;\nint y = 2;\n";
        assert_eq!(
            keep_only_the_lines(1, 1, original, answer),
            "int x = 1;\n   int y = 2;\n",
            "the second line keeps the text it had"
        );
        // Two lines in a longer file: the first and the last are untouched.
        let original = "a\nb\nc\nd\n";
        let answer = "A\nB\nC\nD\n";
        assert_eq!(keep_only_the_lines(2, 3, original, answer), "a\nB\nC\nd\n");
        // A formatter that changed the line count is not spliced by index.
        assert_eq!(
            keep_only_the_lines(1, 1, "a\nb\n", "a b\n"),
            "a b\n",
            "a reflowing formatter has moved the lines, so its answer stands"
        );
        // A range past the end of the file is the file itself.
        assert_eq!(keep_only_the_lines(9, 9, "a\nb\n", "A\nB\n"), "A\nB\n");
    }
    #[test]
    fn a_warning_is_said_once_per_program() {
        assert!(warn_once("clang-format-for-a-test-that-warns-once"));
        assert!(!warn_once("clang-format-for-a-test-that-warns-once"));
        assert!(warn_once("clang-format-for-a-test-that-warns-twice"));
    }
}
