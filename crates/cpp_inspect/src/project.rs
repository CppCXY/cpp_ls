//! **The session a question is asked of** — a project on disk, or a fixture the tool writes and removes.
//!
//! One session type for both: a fixture is written **to disk** under a temporary root and opened the way a real project
//! is. That is deliberate. An in-memory provider would make the fixture's questions go down a different path from the
//! ones a reader asks in an editor, and it shares the `.cppls` cache directory with the real project at `/p` — which is
//! how a fixture's stale summary once made a test fail on a machine where it had passed an hour earlier.

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Args;
use cpp_code_analysis::{DiskFiles, FileView, OpenDocuments, Session, SessionFiles, WatchFilter};

/// Where the question's project and file are.
#[derive(Debug, Clone, Args)]
pub struct ProjectArgs {
    /// The project root. The file is read as part of this project, and its index is what cross-file questions see.
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,
    /// The file the question is about. Relative to `--project`, or absolute.
    #[arg(long, global = true)]
    pub file: Option<PathBuf>,
    /// A fixture: `name=text`, or `name=@path` to read the text from a file on disk. Repeatable; the tool writes them
    /// under a temporary project root and removes it afterwards.
    #[arg(long = "fixture", global = true, value_name = "NAME=TEXT")]
    pub fixtures: Vec<String>,
    /// **Index every project file** instead of only the one being asked about. Slower: a query that needs a class's
    /// declaration reads that file on demand anyway, so this is for questions about the project's *breadth*.
    #[arg(long, global = true)]
    pub index: bool,
    /// With `--index`, take every source file under the project rather than only the one asked about.
    #[arg(long, global = true)]
    pub all_files: bool,
    /// **Build the cooked reading** — the compiler's view of the file, macros expanded and untaken branches gone. Needs
    /// the file's whole include closure, so it is the one expensive step: `diagnostics` requires it, a hover on a
    /// declaration usually does not.
    #[arg(long, global = true)]
    pub cook: bool,
    /// Keep the temporary project a fixture created, and print its path.
    #[arg(long, global = true)]
    pub keep: bool,
    /// Report the setup's timings on stderr.
    #[arg(long, global = true)]
    pub time: bool,
    /// **The database file inside the project's cache directory.** `inspect.db` by default.
    ///
    /// Not the server's `cache.redb`: two writers over one database share a `generation`, and a collection on one side
    /// evicts rows the other is using. The tool must be safe to run while an editor has the same project open — and it
    /// is, because its rows are its own file.
    #[arg(long, global = true, default_value = DEFAULT_DATABASE)]
    pub db: String,
}

/// The database this tool writes unless `--db` says otherwise — see [`ProjectArgs::db`].
pub const DEFAULT_DATABASE: &str = "inspect.db";

/// **What makes one session different from another** — the key a batch run reuses a session by.
///
/// Opening, indexing and cooking a project is the cost, and a series of questions about one file should pay it once.
/// Two requests that agree on all of this can share the session; the ones that disagree cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Setup {
    pub project: std::path::PathBuf,
    pub file: std::path::PathBuf,
    pub index: bool,
    pub all_files: bool,
    pub cook: bool,
    pub db: String,
    pub fixtures: Vec<String>,
}

impl Setup {
    /// The setup a request describes.
    pub fn of(args: &ProjectArgs) -> Setup {
        Setup {
            project: args.project.clone().unwrap_or_default(),
            file: args.file.clone().unwrap_or_default(),
            index: args.index,
            all_files: args.all_files,
            cook: args.cook,
            db: args.db.clone(),
            fixtures: args.fixtures.clone(),
        }
    }
}

/// **A project with one file open and (by default) indexed**, ready to be asked.
pub struct Opened {
    pub session: Session<DiskFiles>,
    pub file: PathBuf,
    pub text: String,
    pub view: FileView,
    /// The temporary root a fixture was written to, removed when this is dropped unless `--keep`.
    temporary: Option<PathBuf>,
    keep: bool,
}

impl Drop for Opened {
    fn drop(&mut self) {
        if let Some(temporary) = &self.temporary
            && !self.keep
        {
            let _ = std::fs::remove_dir_all(temporary);
        }
    }
}

/// Build the session, open the file, and answer with the view the questions are asked of.
pub fn open(args: &ProjectArgs) -> Result<Opened, String> {
    let started = Instant::now();
    let (root, file, text, temporary, fixture_files) = match args.fixtures.is_empty() {
        true => {
            let root = args
                .project
                .clone()
                .ok_or_else(|| "give `--project` and `--file`, or `--fixture`".to_string())?;
            let root = root.canonicalize().map_err(|error| {
                format!("--project {} is not a directory: {error}", root.display())
            })?;
            let named = args.file.clone().ok_or_else(|| {
                "--project needs `--file`: the file the question is about".to_string()
            })?;
            let file = match named.is_absolute() {
                true => named,
                false => root.join(&named),
            };
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
            (root, file, text, None, Vec::new())
        }
        false => {
            let root = std::env::temp_dir().join(format!(
                "cppls-inspect-{}-{}",
                std::process::id(),
                started.elapsed().as_nanos()
            ));
            std::fs::create_dir_all(&root)
                .map_err(|error| format!("cannot create {}: {error}", root.display()))?;
            let mut written: Vec<PathBuf> = Vec::new();
            for fixture in &args.fixtures {
                let (name, body) = fixture.split_once('=').ok_or_else(|| {
                    format!("--fixture wants `name=text` or `name=@path`, not {fixture:?}")
                })?;
                let text = match body.strip_prefix('@') {
                    Some(path) => std::fs::read_to_string(path).map_err(|error| {
                        format!("cannot read the fixture's text {path}: {error}")
                    })?,
                    None => body.replace("\\n", "\n"),
                };
                let relative = name.trim_start_matches(['/', '\\']);
                let path = root.join(relative);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
                }
                std::fs::write(&path, &text)
                    .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
                written.push(path);
            }
            let file = match &args.file {
                Some(named) => match named.is_absolute() {
                    true => named.clone(),
                    false => root.join(named),
                },
                None => written
                    .first()
                    .cloned()
                    .ok_or_else(|| "--fixture wants at least one file".to_string())?,
            };
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("cannot read {}: {error}", file.display()))?;
            (root.clone(), file, text, Some(root), written)
        }
    };

    let opened_at = Instant::now();
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    )
    .with_cache_file({
        // **One leak per process, on purpose.** The cache file name is a `&'static str` because it is hashed into the
        // database's key and lives in the store for the whole session; the tool is short-lived and the name is one
        // string, so leaking it is cheaper than a lifetime parameter threaded through the session for a corner of the
        // API that one caller uses.
        let name: &'static str = Box::leak(args.db.clone().into_boxed_str());
        name
    });
    let opened = opened_at.elapsed();
    let files: Vec<PathBuf> = match args.all_files || args.index {
        // A slice keeps `--index` cheap on a big tree: the *breadth* questions are about the project, and the file
        // asked about is always included.
        true if args.all_files => source_files(&root),
        _ => fixture_files.to_vec(),
    };
    let mut wanted = files;
    if !wanted.iter().any(|known| known == &file) {
        wanted.push(file.clone());
    }
    session.add_project_files(wanted);

    let opened_the_file = Instant::now();
    session.did_open(&file, &text);
    session.catch_up(&file);
    // **The module interface units this file imports, read in** — the half the server does on `didOpen`
    // (`handlers::read_the_modules` + the pump) and this tool used to leave out, so every `std::` name in an
    // `import std;` file answered *nothing* here while the editor showed a card. Measured on `test_module`'s
    // `std_module_demo.cpp` before this: `hover` on `std::vector`, `std::map` and `std::cout` were all `none`.
    let _ = session.read_the_modules_a_file_imports(&file);
    let opened_the_file = opened_the_file.elapsed();

    let cooked = Instant::now();
    if args.cook {
        let _ = session.cook(&file);
    }
    let cooked = cooked.elapsed();

    let indexed = Instant::now();
    if args.index {
        let _ = session.index_everything();
    }
    let indexed = indexed.elapsed();

    let view_at = Instant::now();
    let view = session.view_of_the_file(&file).ok_or_else(|| {
        format!(
            "{} was opened and cooked, and the session has no view of it",
            file.display()
        )
    })?;
    let view_at = view_at.elapsed();

    if args.time {
        eprintln!(
            "[inspect] setup {:.1} ms = open {:.1} + file {:.1} + cook {:.1} + index {:.1} + view {:.1}; {} project file(s); cooked {} indexed {}",
            started.elapsed().as_secs_f64() * 1000.0,
            opened.as_secs_f64() * 1000.0,
            opened_the_file.as_secs_f64() * 1000.0,
            cooked.as_secs_f64() * 1000.0,
            indexed.as_secs_f64() * 1000.0,
            view_at.as_secs_f64() * 1000.0,
            session.project_files().len(),
            args.cook,
            args.index,
        );
    }

    Ok(Opened {
        session,
        file,
        text,
        view,
        temporary,
        keep: args.keep,
    })
}

/// Every source file under a project root, skipping the directories that are not source.
fn source_files(root: &Path) -> Vec<PathBuf> {
    const EXTENSIONS: &[&str] = &[
        "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tcc",
        "cppm", "ixx", "cu", "cuh",
    ];
    const SKIPPED: &[&str] = &[
        ".git",
        ".cppls",
        "target",
        "build",
        "out",
        "Debug",
        "Release",
        ".vs",
        ".idea",
        "node_modules",
    ];
    const LIMIT: usize = 50_000;
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !SKIPPED.contains(&name.as_str()) {
                    pending.push(path);
                }
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    EXTENSIONS
                        .iter()
                        .any(|known| known.eq_ignore_ascii_case(extension))
                });
            if is_source {
                found.push(path);
                if found.len() >= LIMIT {
                    return found;
                }
            }
        }
    }
    found.sort();
    found
}
