//! **How a project that CMake configured gets a compile database.**
//!
//! [`crate::project::discover`] can already read a `compile_commands.json`, and it can already tell when a project
//! *should* have one and does not: a `CMakeCache.txt` whose `CMAKE_EXPORT_COMPILE_COMMANDS` is `OFF`, or is `ON` with
//! no database in the tree. Today both of those become a warning telling the reader to go and run the build — an
//! answer the analysis is in a position to act on, because everything the command needs is already in that cache:
//!
//! ```text
//!   CMAKE_HOME_DIRECTORY          the source directory: what `-S` takes
//!   the cache's own location      the build directory: what `-B` takes
//! ```
//!
//! So this module answers one question — **what command would produce the database, if anything would** — and does
//! not run it. That is the split [`crate::include::toolchain`] makes about compilers: the *policy* (which command,
//! which directory, when it is worth asking) lives here and is tested against a fixed [`CommandRunner`], while the
//! *act* belongs to the layer that owns the user's project and their consent.
//!
//! # Why it is never automatic
//!
//! **Configuring a build runs the project's own code.** A `CMakeLists.txt` may invoke anything, and a language
//! server that configured a project on open would be running a build script because somebody opened a file. What
//! this module produces is therefore a *proposal*: a [`ConfigureCommand`] a caller offers to a person, or runs
//! because it was asked to.
//!
//! # What is proposed, in order
//!
//! ```text
//! 1. a cache that says OFF          configure with -DCMAKE_EXPORT_COMPILE_COMMANDS=ON, into the same build tree
//! 2. a cache that says ON, no cdb   the same: the configure never finished, or the database was cleaned away
//! 3. no cache, but a CMakeLists.txt configure a new build directory beside the source
//! 4. no cache and no CMakeLists     nothing to propose — this is not a CMake project
//! ```
//!
//! Cases 1 and 2 are not guesses: the cache *is* the record of how the project was configured, so re-running with
//! one more `-D` configures what it configured before, into the directory it configured before. Case 3 **is** a
//! guess — a project may be configured into a directory nobody would predict (`-B /tmp/out`), and CMake is the only
//! thing that knows — which is the other reason the answer is a proposal rather than an action.

use std::path::{Path, PathBuf};

use crate::file::paths::FileProvider;
use crate::include::toolchain::CommandRunner;
use crate::project::build::CmakeCache;

/// The source file that makes a directory a CMake project.
pub const CMAKE_SOURCE_FILE: &str = "CMakeLists.txt";

/// The build directory proposed when CMake has never been configured here.
///
/// `build` rather than something cleverer, because it is the name CMake's own documentation uses and the one a
/// person reading the proposal will recognise. It is only ever a **proposal** — see the module note.
pub const PROPOSED_BUILD_DIRECTORY: &str = "build";

/// Why a configure is being proposed, so a caller can say it rather than guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigureReason {
    /// CMake was configured with `CMAKE_EXPORT_COMPILE_COMMANDS=OFF`, so it could have written a database and was
    /// not asked to. The cache says so, and re-running with the flag on is the whole fix.
    TheCacheSaysOff,
    /// The cache says the project exports a database and there is none in the tree. The configure either never
    /// finished or its output was cleaned away; either way re-running writes it.
    NoDatabaseWasWritten,
    /// Nothing has been configured here, and `CMakeLists.txt` says a configure would work. The build directory is
    /// this module's proposal rather than a fact.
    NeverConfigured,
}

impl ConfigureReason {
    /// The reason as a sentence, for a log or a message to a person.
    pub fn message(&self) -> &'static str {
        match self {
            ConfigureReason::TheCacheSaysOff => {
                "CMake was configured with CMAKE_EXPORT_COMPILE_COMMANDS=OFF, so no compile database was written"
            }
            ConfigureReason::NoDatabaseWasWritten => {
                "CMake was configured to export a compile database and none is in the tree"
            }
            ConfigureReason::NeverConfigured => {
                "CMakeLists.txt is here and nothing has been configured, so there is no compile database"
            }
        }
    }
}

/// **The command that would produce a compile database**, and what it is for.
///
/// `program` is a path because a caller may know exactly which `cmake` it wants; the arguments are separate so that
/// a caller can show them, log them, or refuse them, rather than receiving a string it would have to parse back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigureCommand {
    /// `cmake`, resolved by whoever runs it.
    pub program: PathBuf,
    /// The arguments, in order, without the program.
    pub arguments: Vec<String>,
    /// The directory to run in — the source directory, because the arguments name both directories absolutely.
    pub working_directory: PathBuf,
    /// The source directory the configure will read.
    pub source: PathBuf,
    /// The build directory the configure will write, and where the database would appear.
    pub build: PathBuf,
    /// Where the database would be written, so a caller can watch for exactly that file.
    pub database: PathBuf,
    /// Why this is being proposed.
    pub reason: ConfigureReason,
}

impl ConfigureCommand {
    /// The command as one line, for a log or a message.
    ///
    /// Quoted the way a shell would need it rather than the way `argv` holds it, because the only reader is a
    /// person about to paste it.
    pub fn display(&self) -> String {
        let mut line = self.program.display().to_string();
        for argument in &self.arguments {
            line.push(' ');
            if argument.contains(' ') {
                line.push('"');
                line.push_str(argument);
                line.push('"');
            } else {
                line.push_str(argument);
            }
        }
        line
    }
}

/// **What to run to get this project a compile database**, or `None` when nothing would.
///
/// `cache` is what [`crate::project::find_cmake_cache`] found, and `have_a_database` is whether discovery already
/// has one — a caller that has a database has no reason to configure anything, and passing that in keeps the
/// decision here rather than in the caller.
///
/// `files` is read for one thing only: whether `CMakeLists.txt` is in the root, which is what makes case 3 a
/// proposal instead of a guess about a project that may not use CMake at all.
pub fn configure_command(
    files: &impl FileProvider,
    root: &Path,
    cache: Option<&CmakeCache>,
    have_a_database: bool,
) -> Option<ConfigureCommand> {
    if have_a_database {
        return None;
    }

    match cache {
        // **A cache is the record of how this project was configured**, so both of these re-run what was run
        // before, into the directory it wrote before, with one more `-D` on the front.
        Some(cache) => {
            let reason = if cache.exports_compile_commands {
                ConfigureReason::NoDatabaseWasWritten
            } else {
                ConfigureReason::TheCacheSaysOff
            };
            let source = cache.home.clone()?;
            let build = cache.path.parent()?.to_path_buf();
            Some(proposal(&source, &build, reason))
        }
        // **Nothing has been configured**, so the build directory is a proposal. Asked of `CMakeLists.txt` rather
        // than assumed: a project with neither a cache nor a source file is not a CMake project, and telling its
        // reader to run `cmake` would be advice about somebody else's build system.
        None => {
            let source = root.to_path_buf();
            if files.read(&source.join(CMAKE_SOURCE_FILE)).is_none() {
                return None;
            }
            let build = source.join(PROPOSED_BUILD_DIRECTORY);
            Some(proposal(&source, &build, ConfigureReason::NeverConfigured))
        }
    }
}

/// The one shape every proposal takes — see the module note for the three reasons it can carry.
fn proposal(source: &Path, build: &Path, reason: ConfigureReason) -> ConfigureCommand {
    // `-S` and `-B` rather than a working directory and a relative path: a configure is proposed to a person who may
    // run it from anywhere, and the two absolute directories are the whole of what it needs to know. `-D` comes
    // first so that the flag that exists for this module is the first thing read in the command.
    let arguments = vec![
        "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".to_string(),
        "-S".to_string(),
        source.display().to_string(),
        "-B".to_string(),
        build.display().to_string(),
    ];
    ConfigureCommand {
        program: PathBuf::from("cmake"),
        arguments,
        working_directory: source.to_path_buf(),
        source: source.to_path_buf(),
        database: build.join(crate::project::COMPILE_DATABASE_FILE),
        build: build.to_path_buf(),
        reason,
    }
}

/// **Run a proposal**, and say whether a database appeared.
///
/// The one place this module touches a process, kept separate from [`configure_command`] so that the decision can be
/// tested without running anything. `runner` is the caller's — a language server passes the real one, a test passes
/// an answer it chose.
///
/// The answer is read from **the file**, not from the exit status: a configure can succeed and still write nothing,
/// and a caller that trusted the status would then wait for a database that is not coming. `Ok(false)` is that case,
/// and it is not an error — it is a project whose configure does not export a database however it is asked.
pub fn run_configure(
    runner: &impl CommandRunner,
    command: &ConfigureCommand,
    files: &impl FileProvider,
) -> Result<bool, String> {
    let arguments: Vec<&str> = command
        .arguments
        .iter()
        .map(|argument| argument.as_str())
        .collect();
    let Some(output) = runner.run(&command.program, &arguments, &[]) else {
        return Err(format!(
            "{} could not be run: it is not on PATH, or it refused to start",
            command.program.display()
        ));
    };
    if !output.succeeded {
        // The configure's own words are the useful part, and both streams carry them: CMake writes its progress to
        // stdout and its errors to stderr, and which one a failure lands in depends on where it got to.
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim().to_string()
        } else {
            output.stderr.trim().to_string()
        };
        return Err(if detail.is_empty() {
            format!("{} failed with no output", command.display())
        } else {
            detail
        });
    }
    Ok(files.read(&command.database).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::paths::MemoryFiles;
    use crate::include::toolchain::Output;

    /// A `MemoryFiles` holding a source directory, which is all these cases need.
    fn project(files: &[(&str, &str)]) -> MemoryFiles {
        let mut memory = MemoryFiles::new();
        for (path, text) in files {
            memory = memory.with_file(path, *text);
        }
        memory
    }

    /// A cache as CMake would have written it, with the two keys this module reads.
    fn cache(path: &str, home: &str, exports: bool) -> CmakeCache {
        CmakeCache {
            path: PathBuf::from(path),
            home: Some(PathBuf::from(home)),
            exports_compile_commands: exports,
            ..CmakeCache::default()
        }
    }

    /// A runner that answers every command the same way, and remembers what it was asked.
    struct Answers {
        output: std::cell::RefCell<Vec<(String, Vec<String>)>>,
        stdout: String,
        stderr: String,
        succeeded: bool,
    }

    impl Answers {
        fn succeeding(stdout: &str) -> Self {
            Answers {
                output: std::cell::RefCell::new(Vec::new()),
                stdout: stdout.to_string(),
                stderr: String::new(),
                succeeded: true,
            }
        }

        fn failing(stderr: &str) -> Self {
            Answers {
                output: std::cell::RefCell::new(Vec::new()),
                stdout: String::new(),
                stderr: stderr.to_string(),
                succeeded: false,
            }
        }

        fn asked(&self) -> Vec<(String, Vec<String>)> {
            self.output.borrow().clone()
        }
    }

    impl CommandRunner for Answers {
        fn run(
            &self,
            program: &Path,
            arguments: &[&str],
            _environment: &[(std::ffi::OsString, std::ffi::OsString)],
        ) -> Option<Output> {
            self.output.borrow_mut().push((
                program.display().to_string(),
                arguments.iter().map(|a| a.to_string()).collect(),
            ));
            Some(Output {
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
                status: Some(if self.succeeded { 0 } else { 1 }),
                succeeded: self.succeeded,
            })
        }
    }

    #[test]
    fn a_cache_that_says_off_is_re_configured_with_the_flag_on() {
        // The cache is the record of how this project was configured, so the command configures *that* project into
        // *that* build directory — with one more `-D` on the front. Nothing here is a guess.
        let files = project(&[]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false)
            .expect("a cache that says OFF is a proposal");
        assert_eq!(command.reason, ConfigureReason::TheCacheSaysOff);
        assert_eq!(command.source, Path::new("/p"));
        assert_eq!(command.build, Path::new("/p/build"));
        assert_eq!(command.database, Path::new("/p/build/compile_commands.json"));
        assert_eq!(
            command.arguments,
            vec![
                "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON",
                "-S",
                "/p",
                "-B",
                "/p/build"
            ]
        );
    }

    #[test]
    fn a_cache_that_says_on_with_no_database_is_re_configured_the_same_way() {
        // The other half of the warning discovery already produces: the configure never finished, or its output was
        // cleaned. The command is the same one, and the reason is the only difference.
        let files = project(&[]);
        let cache = cache("/p/out/CMakeCache.txt", "/p", true);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false)
            .expect("a cache that says ON with no database is a proposal");
        assert_eq!(command.reason, ConfigureReason::NoDatabaseWasWritten);
        assert_eq!(command.build, Path::new("/p/out"));
    }

    #[test]
    fn nothing_is_proposed_when_a_database_is_already_here() {
        // The caller passes this in rather than being trusted to check first: a project with a database has nothing
        // to configure, and proposing one anyway would offer to re-run a build for no reason.
        let files = project(&[]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        assert_eq!(
            configure_command(&files, Path::new("/p"), Some(&cache), true),
            None
        );
    }

    #[test]
    fn a_source_tree_with_no_cache_is_proposed_a_build_directory_beside_it() {
        // The one guess this module makes, and `CMakeLists.txt` is what keeps it from being advice about somebody
        // else's build system: a project with no cache and no source file is not a CMake project at all.
        let files = project(&[("/p/CMakeLists.txt", "project(p)\n")]);
        let command = configure_command(&files, Path::new("/p"), None, false)
            .expect("a source tree is a proposal");
        assert_eq!(command.reason, ConfigureReason::NeverConfigured);
        assert_eq!(command.build, Path::new("/p/build"));
    }

    #[test]
    fn a_tree_that_is_not_a_cmake_project_is_proposed_nothing() {
        // No cache, no `CMakeLists.txt`: telling this reader to run `cmake` would be advice about a build system
        // they do not use, which is worse than saying nothing.
        let files = project(&[("/p/main.cpp", "int main() {}\n")]);
        assert_eq!(
            configure_command(&files, Path::new("/p"), None, false),
            None
        );
    }

    #[test]
    fn a_cache_with_no_home_directory_is_proposed_nothing() {
        // `find_cmake_cache` already warns about a hand-written cache — one with no `CMAKE_HOME_DIRECTORY`. The
        // source directory is what `-S` takes and there is nothing to derive it from, so there is no command rather
        // than a command naming the wrong directory.
        let files = project(&[]);
        let cache = CmakeCache {
            path: PathBuf::from("/p/build/CMakeCache.txt"),
            home: None,
            ..CmakeCache::default()
        };
        assert_eq!(
            configure_command(&files, Path::new("/p"), Some(&cache), false),
            None
        );
    }

    #[test]
    fn running_a_proposal_asks_for_the_command_it_describes() {
        let files = project(&[("/p/build/compile_commands.json", "[]")]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false).unwrap();
        let runner = Answers::succeeding("");
        assert_eq!(run_configure(&runner, &command, &files), Ok(true));
        assert_eq!(
            runner.asked(),
            vec![(
                "cmake".to_string(),
                vec![
                    "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".to_string(),
                    "-S".to_string(),
                    "/p".to_string(),
                    "-B".to_string(),
                    "/p/build".to_string()
                ]
            )]
        );
    }

    #[test]
    fn a_configure_that_succeeds_without_writing_a_database_is_not_a_database() {
        // **Read from the file, not from the status.** A configure can exit 0 and write nothing — a project that
        // does not export a database however it is asked — and a caller that trusted the status would then wait for
        // a file that is not coming.
        let files = project(&[]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false).unwrap();
        let runner = Answers::succeeding("-- Configuring done\n");
        assert_eq!(run_configure(&runner, &command, &files), Ok(false));
    }

    #[test]
    fn a_failed_configure_carries_the_programs_own_words() {
        // Both streams carry them: CMake writes progress to stdout and errors to stderr, and which one a failure
        // lands in depends on where it got to.
        let files = project(&[]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false).unwrap();
        let runner = Answers::failing("CMake Error: could not find a build system\n");
        assert_eq!(
            run_configure(&runner, &command, &files),
            Err("CMake Error: could not find a build system".to_string())
        );
    }

    #[test]
    fn the_command_reads_back_as_one_line_a_person_could_paste() {
        let files = project(&[]);
        let cache = cache("/p/build/CMakeCache.txt", "/p", false);
        let command = configure_command(&files, Path::new("/p"), Some(&cache), false).unwrap();
        assert_eq!(
            command.display(),
            "cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -S /p -B /p/build"
        );
    }
}
