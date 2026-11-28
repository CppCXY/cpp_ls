//! **What the tool is told to do** — the command line, and the file set it resolves to.
//!
//! Two ways to name the work:
//!
//! ```text
//! cpp_check [ROOT]                       walk the workspace (translation units, or --headers too)
//! cpp_check ROOT --file a.c --file b.c   exactly these files
//! cpp_check ROOT --files-from list.txt   exactly the files the list names
//! ```
//!
//! **Why the second form exists.** C++'s locality is strong: a change to four files wants those four files checked, not
//! the 228 translation units of the project, each of which pays for its own include closure (measured on neovim: ~0.8 s
//! of render per file). A pre-commit hook, an editor mapping "check this buffer", and a person reproducing a report
//! all know which files they mean, and telling the tool is cheaper than making it ask again.
//!
//! The project is still `ROOT`: the compile database, the toolchain's include search and `.cppls.toml` all come from
//! there, so a named file is read the way the build reads it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;

use crate::walk;

#[derive(Parser, Debug)]
#[command(
    name = "cpp_check",
    version,
    about = "Run the analysis's checks over a whole workspace.",
    long_about = None
)]
pub struct Args {
    /// The workspace the files belong to — its compile database, flags and configuration. Defaults to the current
    /// directory.
    #[arg(default_value = ".")]
    pub root: PathBuf,

    /// Check only this file. Repeatable; the project still comes from ROOT.
    #[arg(long = "file", value_name = "PATH")]
    pub files: Vec<PathBuf>,

    /// A file naming the files to check, one path per line. Blank lines and lines starting with `#` are ignored, and
    /// a relative path is relative to the current directory.
    #[arg(long = "files-from", value_name = "PATH")]
    pub files_from: Option<PathBuf>,

    /// Report only findings at or above this severity: hint, warning or error.
    #[arg(long, value_enum, default_value_t = Level::Warning)]
    pub min_severity: Level,

    /// Only these codes, by name — `--code=unresolved-call-or-receiver,use-after-free`.
    #[arg(long, value_delimiter = ',')]
    pub code: Vec<String>,

    /// Skip these codes, by name.
    #[arg(long = "skip-code", value_delimiter = ',')]
    pub skip_code: Vec<String>,

    /// Also check headers on their own. By default only translation units are checked — a header is checked in the
    /// context of the files that include it, which is the only context it compiles in.
    #[arg(long)]
    pub headers: bool,

    /// Stop after this many files. A smoke run, not a check.
    #[arg(long)]
    pub max_files: Option<usize>,

    /// Machine-readable output: one JSON object.
    #[arg(long)]
    pub json: bool,

    /// Print nothing but the findings and the summary — no progress lines.
    #[arg(long)]
    pub quiet: bool,

    /// A path substring to skip, repeatable — `--exclude=third_party`.
    #[arg(long)]
    pub exclude: Vec<String>,

    /// Print where the time went, stage by stage.
    #[arg(long)]
    pub time: bool,

    /// **Read the whole project before the first file** — what the workspace-wide rules need.
    ///
    /// `missing-include`, `pure-virtual-not-overridden` and `unused` ask the *index* about files the checked file
    /// never includes, and a per-file closure cannot answer that: measured on the corpus, `missing-include` said
    /// "`OnlyHere` is declared in only_here.h" from `cpp_inspect --index` and nothing at all from `cpp_check`, because
    /// `only_here.h` was not in the closure the run read. The price is the project`s reading, so it is opt-in.
    #[arg(long)]
    pub index: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Level {
    Hint,
    Warning,
    Error,
}

impl Level {
    pub fn rank(self) -> u8 {
        match self {
            Level::Hint => 0,
            Level::Warning => 1,
            Level::Error => 2,
        }
    }

    pub fn of(severity: cpp_code_analysis::sema::check::RuleSeverity) -> Level {
        use cpp_code_analysis::sema::check::RuleSeverity;
        match severity {
            RuleSeverity::Error => Level::Error,
            RuleSeverity::Warning => Level::Warning,
            RuleSeverity::Hint | RuleSeverity::Information => Level::Hint,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Level::Hint => "hint",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

/// **The files a run checks**, in the order it checks them.
///
/// `--file` and `--files-from` name the set exactly, and the walk is skipped: the point of naming them is that the
/// project is too large to read for four files. Without either, the workspace is walked — and `--max-files` caps the
/// result, which is a smoke run rather than a check.
///
/// A named path is **absolute-ized but not canonicalized**, for the reason `main` documents about `ROOT`: on Windows
/// `canonicalize` answers `\\?\C:\…`, and the analysis keys everything on the spelling a client writes.
pub fn files_to_check(root: &Path, args: &Args) -> Result<Vec<PathBuf>, String> {
    let named = args.files.iter().cloned().chain(
        args.files_from
            .as_deref()
            .map(paths_in)
            .transpose()?
            .unwrap_or_default(),
    );

    let mut files: Vec<PathBuf> = named
        .map(|path| {
            std::path::absolute(&path)
                .map_err(|error| format!("cannot resolve {}: {error}", path.display()))
        })
        .collect::<Result<_, _>>()?;

    if files.is_empty() {
        files = walk::discover(root, args.headers, &args.exclude);
    } else {
        // **One entry per file.** A list that names a file twice (a hook that appends the changed files to a base
        // list) should check it once, and the order is the reader's: sorted, like the walk's.
        files.sort();
        files.dedup();
    }

    if let Some(max) = args.max_files {
        files.truncate(max);
    }
    Ok(files)
}

/// The paths a `--files-from` list names, in the order it writes them.
///
/// `#` is a comment because a list in a repository is kept next to a reason; a blank line is a separator. A missing
/// list is an error rather than an empty set, because "check these files" answered with "no files" is the wrong
/// silence.
fn paths_in(list: &Path) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(list)
        .map_err(|error| format!("cannot read {}: {error}", list.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect())
}

/// A timer the run's summary needs, kept here so `main` reads as the sequence of steps rather than as bookkeeping.
pub struct Clock(Instant);

impl Clock {
    pub fn start() -> Clock {
        Clock(Instant::now())
    }

    pub fn seconds(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            root: PathBuf::from("."),
            files: Vec::new(),
            files_from: None,
            min_severity: Level::Warning,
            code: Vec::new(),
            skip_code: Vec::new(),
            headers: false,
            max_files: None,
            json: false,
            quiet: true,
            exclude: Vec::new(),
            time: false,
            index: false,
        }
    }

    /// **A list file, as a repository writes one**: paths, comments, blank lines, and a repeated path.
    #[test]
    fn a_file_list_names_the_files_and_ignores_comments() {
        let directory = std::env::temp_dir().join(format!("cpp_check_list_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a temp dir");
        let list = directory.join("files.txt");
        std::fs::write(
            &list,
            "# the files this change touches\nsrc/one.c\n\nsrc/two.c   \n  # indented comment\nsrc/one.c\n",
        )
        .expect("the list");

        let mut args = args();
        args.files_from = Some(list);
        let files = files_to_check(&directory, &args).expect("the list resolves");
        let names: Vec<String> = files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["one.c", "two.c"],
            "two files, sorted, with the repeat and the comments gone"
        );
        assert!(
            files.iter().all(|path| path.is_absolute()),
            "a named path is absolute-ized, so the session's keys do not depend on the caller's directory"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A list that cannot be read is an error, not an empty check** — "check these files" answered with "nothing"
    /// is a silence a hook would believe.
    #[test]
    fn a_missing_list_is_an_error() {
        let mut args = args();
        args.files_from = Some(PathBuf::from("no-such-list-anywhere.txt"));
        let answer = files_to_check(Path::new("."), &args);
        assert!(
            answer.expect_err("a missing list").contains("cannot read"),
            "the message says which file could not be read"
        );
    }

    /// **`--file` wins over the walk**, and `--max-files` still caps the set.
    #[test]
    fn a_named_file_is_checked_instead_of_the_walk() {
        let directory =
            std::env::temp_dir().join(format!("cpp_check_named_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a temp dir");
        for name in ["a.c", "b.c", "c.c"] {
            std::fs::write(directory.join(name), "int x;\n").expect("a fixture file");
        }

        let mut args = args();
        args.root = directory.clone();
        args.files = vec![directory.join("b.c")];
        let files = files_to_check(&directory, &args).expect("the named file");
        assert_eq!(files.len(), 1, "the walk did not run: {files:?}");
        assert_eq!(files[0].file_name().unwrap(), "b.c");

        args.files.push(directory.join("a.c"));
        args.max_files = Some(1);
        let files = files_to_check(&directory, &args).expect("both named files");
        assert_eq!(files.len(), 1, "`--max-files` caps a named set too");

        let _ = std::fs::remove_dir_all(&directory);
    }
}
