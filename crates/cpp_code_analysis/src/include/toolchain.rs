//! Finding the compiler, and asking it where **its own** headers are.
//!
//! `#include <vector>` is the first include in most real files, and until it resolves, nothing else about that
//! file is usable: `index::store` refuses to cache a summary whose includes did not resolve, so a project whose
//! standard library is invisible is a project with no cache at all. That is what this module is for — and it is
//! deliberately the *only* thing it is for.
//!
//! # Why the compiler is asked instead of worked out
//!
//! The directories a toolchain searches are not derivable from anything the analysis can see. They depend on how
//! the compiler was configured (`--prefix`, `--with-sysroot`), on its version, on the target, and on environment
//! variables — the one measured here is `…/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++`, which encodes the
//! target triple and the version and could not have been guessed. So the compiler is **asked**: `-E -v` prints
//! its search list, and the list is taken as it is printed.
//!
//! ```text
//! #include "..." search starts here:
//! #include <...> search starts here:
//!  C:/…/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++
//!  C:/…/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/x86_64-w64-mingw32
//! End of search list.
//! ```
//!
//! Both compilers measured here print that shape — GCC 15.1.0 (MinGW-w64) and Clang 18 — so the parser keys on
//! the two markers rather than on either compiler's spelling. The entries are **not** cleaned up beyond
//! normalization: they are printed with `..` in them (`bin/../lib/gcc/…`), and normalizing is what makes two
//! spellings of one directory compare equal, which is all the resolver needs.
//!
//! # What is not asked, and why that is the whole of P0
//!
//! `-D`, `-U` and `-std` are **not** put on the command line. They change what the code means rather than where
//! it is, and feeding them in would mean the macro environment becomes part of what a summary depends on — which
//! `docs/index-design.md` records as a change that must bring the macro environment back into the key and bump
//! `FORMAT_VERSION`. `docs/std-library.md` calls that the expensive layer and keeps it separate on purpose.
//!
//! # Why the answer is not cached on disk
//!
//! Asking costs **84 ms** (measured, GCC 15.1.0, five runs) — once per session. Writing it down would need a key
//! for "which compiler is this", and a stale include path is a *wrong* answer rather than a slow one: it would
//! make `#include <vector>` resolve into a directory that no longer holds it, and every declaration after it
//! would be attributed to the wrong file. The measured cost does not buy that risk, so the answer lives in memory
//! and is re-asked next session. (See `docs/std-library.md`: the same reasoning is applied there to the decision
//! *not* to index the standard library ahead of time.)
//!
//! # MSVC is deliberately not handled
//!
//! `cl` has no `-v`, and its include directories come from `INCLUDE`, which `vcvarsall.bat` sets in the shell
//! that runs the build — an editor started from anywhere else does not have it. There is no `cl` on the machine
//! this was written on, so there is also no evidence about what it prints. Writing that path now would be a guess
//! with a plausible shape, which is worse than a gap that says so.

use std::path::{Path, PathBuf};

use cpp_parser::Dialect;

use crate::include::config::{CommandLineMacro, CompileCommands, CompilerConfig, IncludePath};
use crate::file::paths::{FileProvider, normalize_path};

/// What a program wrote to its two streams.
///
/// Both, because the answer is not always on the same one: GCC and Clang print the search list to **stderr**,
/// and a caller that kept only stdout would find nothing and report "no compiler" for a compiler that answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// The exit status, or `None` when the process was killed by a signal.
    ///
    /// Load-bearing for one compiler: `cl` has no `-v`, so the only signal that distinguishes "could not be asked"
    /// from "asked and the answer is empty" is the status. Measured: `1` is the environment not obtained, `2` is
    /// the command line rejected or headers missing, `4` is a resource failure, `0` is success
    /// (`docs/msvc-notes.md`, *Failure modes*). A caller that read only the text would treat a silent exit 1 as
    /// "this compiler predefines nothing".
    pub status: Option<i32>,
    pub succeeded: bool,
}

impl Output {
    /// Both streams as one string, which is what a marker-delimited answer needs.
    pub fn combined(&self) -> String {
        let mut all = self.stdout.clone();
        all.push_str(&self.stderr);
        all
    }
}

/// Runs a program. The second thing in this crate that touches the world outside it.
///
/// A trait for the same reason [`FileProvider`] is one: what is worth testing is the *policy* (which compiler to
/// ask) and the *parsing* (what its answer means), and both must be testable on a machine with no compiler
/// installed — or with a different one. A test suite that needs `g++` on `PATH` is a test suite that fails on a
/// machine that has only `clang++`.
pub trait CommandRunner {
    /// Run `program` with `arguments`, an **empty standard input**, and these extra environment variables.
    ///
    /// `None` covers every reason the answer is unavailable — not found, not executable, no permission, a
    /// non-zero exit that means the program refused to run — because they all lead to the same decision: this
    /// compiler cannot be asked, so try the next one. A caller that needs to tell those apart reads the exit
    /// status itself, which is why [`Output`] carries it.
    ///
    /// The environment is a parameter rather than the process's own because one toolchain needs it: `cl` finds its
    /// headers through `INCLUDE` and has no `-E -v` to ask, so the analysis **sets `INCLUDE` itself** and runs the
    /// compiler with it (`docs/msvc-notes.md` measures that this works, and that going through `vcvars64.bat`
    /// instead costs ~1.3 s against ~50 ms).
    fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Option<Output>;
}

/// The real thing.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskCommands;

impl CommandRunner for DiskCommands {
    fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Option<Output> {
        let mut command = std::process::Command::new(program);
        command
            .args(arguments)
            // An empty translation unit, spelled `-` rather than `NUL` or `/dev/null`: the compiler reads its
            // input from standard input, which needs no file to exist and no platform to be known. (`cl` does not
            // accept stdin at all — it is given a real file — but the flag is what a GNU-like compiler expects.)
            .stdin(std::process::Stdio::null());

        for (name, value) in environment {
            command.env(name, value);
        }

        let output = command.output().ok()?;

        Some(Output {
            // Lossy rather than an error: compiler output is ASCII except for the paths in it, and a path with a
            // byte that is not UTF-8 is still a path worth trying. (It is also *localized*: every `cl` diagnostic
            // on the machine this was written on is Chinese, so no caller may match on message text — match on the
            // exit status and on the `D####`/`C####` codes, as `docs/msvc-notes.md` records.)
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            status: output.status.code(),
            succeeded: output.status.success(),
        })
    }
}

/// The environment a compiler is looked for in, as data.
///
/// Data rather than direct `std::env` calls, for two reasons. The small one is that a test for "`CXX` beats
/// `PATH`" should not have to mutate the process environment, which is shared with every other test running at
/// the same time. The large one is that this makes the policy **readable**: the order below is the whole answer to
/// "which compiler", and it can be read without knowing what a `Command` is.
///
/// The same reasoning as [`CompilerConfig`]: the configuration is supplied, and what is supplied here is the
/// process environment, read once by [`Environment::current`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `CXX` — the name or path of the C++ compiler, which is the one the build uses.
    pub cxx: Option<PathBuf>,
    /// `CC` — the C compiler, asked only when `CXX` says nothing.
    ///
    /// Worth asking because a C++ project's compiler and its C compiler are usually the same toolchain, and a
    /// toolchain that can answer for C can answer for C++ (`gcc -x c++ -E -v` lists the C++ directories too).
    pub cc: Option<PathBuf>,
    /// `PATH`, in order.
    pub path: Vec<PathBuf>,
    /// `PATHEXT` on Windows (`;.EXE;.BAT;…`), empty elsewhere.
    ///
    /// Empty means "the platform's rule is in the standard library" — on Unix an executable is one with the
    /// execute bit, and no suffix has to be tried.
    pub path_extensions: Vec<String>,
}

impl Environment {
    /// The process environment.
    pub fn current() -> Self {
        Environment {
            cxx: std::env::var_os("CXX").map(PathBuf::from),
            cc: std::env::var_os("CC").map(PathBuf::from),
            path: std::env::var_os("PATH")
                .map(|value| std::env::split_paths(&value).collect())
                .unwrap_or_default(),
            // Split on `;` and drop the leading dot: `PATHEXT` is written `.COM;.EXE;.BAT`, and what a caller
            // wants is the suffixes, in the order Windows tries them.
            path_extensions: std::env::var("PATHEXT")
                .map(|value| {
                    value
                        .split(';')
                        .map(|extension| extension.trim().to_string())
                        .filter(|extension| !extension.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// The compilers tried on `PATH`, in the order of how likely they are to be the right one for C++.
///
/// `c++` is on the list because it is the one name POSIX guarantees to exist and to be the C++ compiler; the
/// others are the ones people actually install. `cl` is not here — see the module documentation.
const COMPILER_NAMES: &[&str] = &["g++", "clang++", "c++", "gcc", "clang", "cc"];

/// The compiler to ask, or `None` when nothing usable was found.
///
/// # The order, and why it is this one
///
/// ```text
/// 1. the compiler compile_commands.json names for this file  — it is what actually builds the file
/// 2. `CXX`, then `CC`                                        — the project's stated choice
/// 3. `g++`, `clang++`, `c++`, … on `PATH`                    — the machine's default
/// ```
///
/// Each step is a weaker claim than the one before it. The build database is the only one that knows *this* file
/// is built by *that* compiler — a project with two toolchains in it is what step 1 exists for — while `PATH` is
/// a guess about the machine and is last because of it.
///
/// A name with a separator in it is a path and is looked for where it says (`CXX=/opt/gcc/bin/g++`); a bare name
/// is searched on `PATH`, which is what a shell would do with it.
pub fn find_compiler(
    files: &impl FileProvider,
    commands: Option<&CompileCommands>,
    for_file: &Path,
    environment: &Environment,
) -> Option<PathBuf> {
    if let Some(commands) = commands
        && let Some(command) = commands.command_for(for_file)
        && let Some(program) = command.arguments.first()
    {
        let named = PathBuf::from(program);
        // The database writes the command as the build ran it, which may be a bare `g++` that only exists on the
        // build's `PATH` — so a bare name here is searched like any other, rather than reported as missing.
        if let Some(found) = locate(files, &named, environment) {
            return Some(found);
        }
    }

    for variable in [environment.cxx.as_ref(), environment.cc.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(found) = locate(files, variable, environment) {
            return Some(found);
        }
    }

    COMPILER_NAMES
        .iter()
        .find_map(|name| locate(files, Path::new(name), environment))
}

/// Find one name: as a path when it says where it is, on `PATH` when it does not.
fn locate(files: &impl FileProvider, name: &Path, environment: &Environment) -> Option<PathBuf> {
    if name.components().count() > 1 {
        return files.exists(name).then(|| name.to_path_buf());
    }

    let file_name = name.to_string_lossy().to_string();
    for directory in &environment.path {
        for candidate in candidates_in(directory, &file_name, &environment.path_extensions) {
            if files.exists(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

/// The spellings of `name` inside `directory`, in the order the platform tries them.
///
/// With extensions, the bare name is **not** tried first: `g++` next to a file literally named `g++` and a
/// `g++.exe` is a case Windows itself resolves in favour of the extension, and trying the bare name first would
/// find a file the system would not run.
fn candidates_in(directory: &Path, name: &str, extensions: &[String]) -> Vec<PathBuf> {
    if extensions.is_empty() {
        return vec![directory.join(name)];
    }

    extensions
        .iter()
        .map(|extension| directory.join(format!("{name}{extension}")))
        .collect()
}

/// Ask a compiler where it looks for its own headers, **and what it predefines**.
///
/// `None` when the list could not be obtained: the compiler did not run, or its output had no search list in it
/// (which is what an unrelated program with the same name prints). Both mean the same thing to a caller — this
/// toolchain cannot be asked — so they are one answer, and the caller keeps the configuration it had.
///
/// # Why the macro table comes from the same call
///
/// `-dM` makes the preprocessor print every macro it predefines instead of preprocessing, so the *same* process
/// that prints the search list also prints `_WIN32`, `__x86_64__`, `__cplusplus` and the other 480 names that no
/// header defines. Those names are what a condition like `#ifdef _WIN32` is about, and there is nowhere else to get
/// them: they are the compiler's, not the project's. One process, because the two answers have the same source and
/// a second invocation would be a second chance to ask a different compiler.
/// # Why the standard is passed
///
/// `-dM` prints `__cplusplus` with the value of the language version the compiler was **invoked** for, so asking
/// without `-std=` gets the default (`201703L` for this `g++`) while the file being read may be built as C++20.
/// A condition like `#if __cplusplus >= 202002L` would then come out wrong — not unknown, *wrong*, which is the
/// failure mode this whole layer is built to avoid. So the standard comes from the same place the include paths do:
/// the compile database's entry for the file.
pub fn search_paths(
    runner: &impl CommandRunner,
    compiler: &Path,
    standard: Option<&str>,
) -> Option<Toolchain> {
    // `-E` stops after preprocessing, `-v` prints what the driver is doing, `-x c++` says which language's
    // directories to list (without it, `g++` would still list C++'s, but `gcc` would list C's), and `-` reads an
    // empty translation unit from standard input. `-dM` adds the predefined macros to that output.
    let language = standard.map(|standard| format!("-std={standard}"));
    let mut arguments = vec!["-dM", "-E", "-v", "-x", "c++"];
    if let Some(language) = &language {
        arguments.push(language);
    }
    arguments.push("-");

    let output = runner.run(compiler, &arguments, &[])?;
    let combined = output.combined();

    let system_include_paths = parse_search_list(&combined);

    if system_include_paths.is_empty() {
        return None;
    }

    let builtin_macros = parse_builtin_macros(&combined);
    let dialect = Dialect::from_predefined_macros(
        builtin_macros
            .iter()
            .map(|define| (define.name.as_ref(), define.value.as_deref())),
    );

    Some(Toolchain {
        compiler: Some(compiler.to_path_buf()),
        version: parse_version(&combined),
        system_include_paths,
        builtin_macros,
        dialect,
        // Overwritten by the caller that knows which step of the order found this compiler.
        source: ToolchainSource::Path,
        note: None,
    })
}

/// The macros a compiler predefines, from `-dM`'s `#define NAME value` lines.
///
/// A macro with no value is stored with an empty one, which is what it has: `#define __linux` and
/// `#define __cplusplus 202002L` are the same kind of fact with different bodies, and a consumer asking
/// `defined(NAME)` only needs the first half.
///
/// Lines that are not `#define` are ignored, which is what lets one output carry both this and the search list —
/// the version line, the `#include <…> search starts here:` markers and the rest of `-v`.
pub fn parse_builtin_macros(output: &str) -> Vec<CommandLineMacro> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter_map(|rest| {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next()?.trim();
            // A function-like macro (`#define __INT_MAX__ 2147483647` is object-like; `#define f(x) …` is not,
            // and its `(x)` is glued to the name in this output) is skipped: it is not a value a `#if` can use, and
            // a name with parentheses in it would be a name no condition ever tests.
            if name.is_empty() || name.contains('(') {
                return None;
            }

            let value = parts.next().unwrap_or("").trim();
            Some(CommandLineMacro {
                name: name.into(),
                value: (!value.is_empty()).then(|| value.into()),
            })
        })
        .collect()
}

/// Find a compiler and ask it. The one call a caller needs.
///
/// # The order, and what each step is worth
///
/// ```text
/// 1. compile_commands.json's compiler for this file   the project saying which compiler builds it
/// 2. `CXX`, then `CC`                                 the person saying which compiler
/// 3. the platform's own: MSVC on Windows              the machine's convention — see `docs/msvc-notes.md`
/// 4. `g++`, `clang++`, `c++`, … on `PATH`             the machine's default
/// 5. the system's header directories                  nothing could be asked: headers without macros
/// ```
///
/// Each step is a weaker claim than the one before it, and the answer says which one it came from
/// ([`Toolchain::source`]) — because "the analysis used MinGW's headers for an MSVC project" is a question that
/// gets asked, and the answer has to be in the report rather than reconstructed from a log.
///
/// **Windows prefers MSVC** unless the project or the person said otherwise: a Windows project's standard library
/// is the one that ships with the toolchain the build uses, and a `g++` on `PATH` (MinGW, say) has a *different*
/// libstdc++ and a different Windows SDK — analysing an MSVC project against it answers about declarations the
/// build never sees.
///
/// `None` when nothing at all could be found: no compiler, and not even a conventional header directory. That is a
/// state the analysis reports (every include stays unresolved) rather than papering over.
pub fn discover(
    files: &impl FileProvider,
    runner: &impl CommandRunner,
    commands: Option<&CompileCommands>,
    for_file: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
) -> Option<Toolchain> {
    // The database's own compiler is the one candidate this entry point can work out for itself, and it is the
    // first one: the project saying which compiler builds *this* file. A caller that also knows what `.cppls.toml`
    // and `CMakeCache.txt` say uses [`discover_with`], which takes the whole ordered list — including this one,
    // because it is the caller that knows where it ranks (`.cppls.toml` above it, CMake below).
    let from_database = database_compiler(commands, for_file)
        .map(|compiler| (compiler, ToolchainSource::CompileDatabase));

    discover_with(
        files,
        runner,
        from_database.as_slice(),
        commands,
        for_file,
        environment,
        layout,
    )
}

/// The compiler a compile database names for a file, as a candidate.
///
/// `commands.command_for(for_file)` rather than the first entry: a database is per-file, and asking about a file
/// no entry mentions is how a machine with two toolchains gets the wrong compiler for the project's own headers —
/// which is what the tier *below* (`CXX`, then the platform) is for.
fn database_compiler(commands: Option<&CompileCommands>, for_file: &Path) -> Option<PathBuf> {
    commands
        .and_then(|commands| commands.command_for(for_file))
        .and_then(|command| command.arguments.first())
        .map(PathBuf::from)
}

/// [`discover`] with compilers the *project* named, tried before the environment's.
///
/// The three sources that rank above `CXX` are all "the project said so", and they are passed in rather than looked
/// up here because each is read by a different layer: `.cppls.toml` and `compile_commands.json` by
/// [`crate::project`], `CMakeCache.txt` by [`crate::project::build`]. What this function owns is the **order**,
/// which is the one thing that has to be in one place:
///
/// ```text
/// 1. the names passed in, in the order given      .cppls.toml > compile database > CMakeCache
/// 2. `CXX`, then `CC`                             the person's
/// 3. the platform's own: MSVC on Windows          the machine's convention
/// 4. `g++`, `clang++`, … on `PATH`                the machine's default
/// 5. the system's header directories              nothing to ask
/// ```
///
/// A candidate that cannot be found or does not answer is skipped, not fatal: a project that names a compiler the
/// machine does not have is better analysed with the machine's own — and [`Toolchain::source`] says which one
/// answered, so the report shows that the project's choice was not available.
pub fn discover_with(
    files: &impl FileProvider,
    runner: &impl CommandRunner,
    named: &[(PathBuf, ToolchainSource)],
    commands: Option<&CompileCommands>,
    for_file: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
) -> Option<Toolchain> {
    let standard = standard_for(commands, for_file);

    // 1. The project's own answers.
    for (name, source) in named {
        if let Some(toolchain) = ask(
            files,
            runner,
            name,
            environment,
            layout,
            standard.as_deref(),
        ) {
            return Some(toolchain.claiming(*source));
        }
    }

    // 2. The environment's.
    for variable in [environment.cxx.as_ref(), environment.cc.as_ref()]
        .into_iter()
        .flatten()
    {
        if let Some(toolchain) = ask(
            files,
            runner,
            variable,
            environment,
            layout,
            standard.as_deref(),
        ) {
            return Some(toolchain.claiming(ToolchainSource::Environment));
        }
    }

    // 3. The platform's own toolchain.
    if let Some(toolchain) = ask_msvc(runner, layout, standard.as_deref(), None) {
        return Some(toolchain.claiming(ToolchainSource::PlatformDefault));
    }

    // 4. Whatever is installed.
    for name in COMPILER_NAMES {
        if let Some(toolchain) = ask(
            files,
            runner,
            Path::new(name),
            environment,
            layout,
            standard.as_deref(),
        ) {
            return Some(toolchain.claiming(ToolchainSource::Path));
        }
    }

    // 5. Nothing to ask: the headers a convention promises, and no macro table at all.
    system_headers_fallback(runner, layout)
}

/// The standard the file is built with, from the database's entry for it.
///
/// It decides the value of `__cplusplus` in a compiler's predefined table, and with it every
/// `#if __cplusplus >= …` in every header. Asking a compiler for its default instead would answer a question
/// nobody asked, with a number that looks right.
fn standard_for(commands: Option<&CompileCommands>, for_file: &Path) -> Option<String> {
    commands
        .and_then(|commands| commands.command_for(for_file))
        .and_then(|command| command.to_config().standard)
        .map(|standard| standard.to_string())
}

/// Ask one compiler by name, whichever kind it is.
fn ask(
    files: &impl FileProvider,
    runner: &impl CommandRunner,
    name: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
    standard: Option<&str>,
) -> Option<Toolchain> {
    let found = locate(files, name, environment)?;

    if is_msvc(&found) {
        return ask_msvc(runner, layout, standard, Some(found));
    }

    search_paths(runner, &found, standard)
}

/// Is this Microsoft's compiler?
///
/// By name, because the two kinds are asked in completely different ways and a compiler's name is the only thing
/// an analysis can know before running it: `cl` has no `-E -v`, and its search list comes from `INCLUDE`. A
/// program called `cl` that is not Microsoft's would fail the macro dump and be reported as a compiler that could
/// not be asked — which is the honest outcome, and not a silent wrong answer.
fn is_msvc(program: &Path) -> bool {
    program
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("cl"))
}

/// Ask Microsoft's compiler: build its search list, then dump its macros.
///
/// The macro dump can fail on its own (`/PD` needs `/Zc:preprocessor`; the machine may have no resources for the
/// language it is running in), and that is **not** the same as having no toolchain: the include directories are
/// still the right ones, and a project whose headers resolve is a project half the queries work for. So a failure
/// is reported in [`Toolchain::note`] and the toolchain is returned anyway — with no macros, which leaves every
/// condition naming one of them `Unknown` rather than wrong.
fn ask_msvc(
    runner: &impl CommandRunner,
    layout: &crate::include::msvc::WindowsLayout,
    standard: Option<&str>,
    named: Option<PathBuf>,
) -> Option<Toolchain> {
    if !cfg!(windows) {
        return None;
    }

    let mut msvc = crate::include::msvc::discover(runner, layout)?;
    if let Some(named) = named {
        msvc.toolset.cl = named;
    }

    let macros = crate::include::msvc::predefined_macros(runner, &msvc, standard);

    let (builtin_macros, version, note) = match macros {
        Some(macros) => {
            let version = crate::include::msvc::version_line(&msvc, &macros);
            (macros, Some(version), None)
        }
        None => (
            Vec::new(),
            Some(format!("MSVC {}", msvc.toolset.version)),
            Some(
                "the compiler could not be asked for its predefined macros, so conditions that name one \
                 (``_MSC_VER``, ``_WIN32``, ``__cplusplus``) are Unknown"
                    .to_string(),
            ),
        ),
    };

    Some(Toolchain {
        compiler: Some(msvc.toolset.cl.clone()),
        version,
        system_include_paths: msvc.include_paths(),
        builtin_macros,
        // Knowing the compiler is `cl` is knowing the dialect, whether or not it answered: MSVC's reading of
        // `__int128`, of attributes and of the preprocessor's own syntax is what the parser needs to know.
        dialect: Some(Dialect::Msvc),
        source: ToolchainSource::PlatformDefault,
        note,
    })
}

/// The last resort: header directories a convention promises, with no compiler and no macros.
fn system_headers_fallback(
    runner: &impl CommandRunner,
    layout: &crate::include::msvc::WindowsLayout,
) -> Option<Toolchain> {
    let found = crate::include::system_headers::discover(runner, layout)?;

    Some(Toolchain {
        compiler: None,
        version: None,
        system_include_paths: found.directories,
        builtin_macros: Vec::new(),
        dialect: None,
        source: ToolchainSource::SystemHeaders,
        note: Some(format!(
            "no compiler could be asked; using the system's own header directories ({}), so compiler macros \
             are Unknown",
            found.based_on
        )),
    })
}

/// A compiler that answered, and what it said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// The compiler that was asked, as it was found. `None` in the last-resort case, where no compiler could be
    /// asked at all and only the system's conventional header directories are known.
    pub compiler: Option<PathBuf>,
    /// Its version line, when it printed one: `gcc version 15.1.0 (…)`, `clang version 18.1.8`,
    /// `MSVC 14.35.32215 (_MSC_FULL_VER 193532215)`.
    ///
    /// Kept for a human, not for a decision: when the analysis of a project is wrong because it was read against
    /// the wrong toolchain, the first question is which toolchain it was read against.
    pub version: Option<String>,
    /// The directories it searches for `#include <…>`, in its own order, normalized with case **preserved** —
    /// they are directories to open, not keys to compare. See [`parse_search_list`].
    pub system_include_paths: Vec<PathBuf>,
    /// Every macro the compiler **predefines**, as `#define` lines from `-dM` gave them.
    ///
    /// The half of the macro environment that is not in any file: `_WIN32`, `__x86_64__`, `__cplusplus` and about
    /// 480 more. A condition like `#ifdef _WIN32` or `#if __cplusplus >= 201703L` is a question about these, and
    /// there is nowhere else to get the answer. See [`parse_builtin_macros`], and
    /// [`Toolchain::macros`] for how they are handed to a condition evaluator.
    ///
    /// **Empty is not "none of them are defined"**: it means the compiler could not be asked, which
    /// [`Toolchain::note`] says out loud and which makes every condition naming one of these `Unknown`.
    pub builtin_macros: Vec<CommandLineMacro>,
    /// The dialect the parser should read the project's files in, when the toolchain settles it.
    ///
    /// From the macro table when there is one (`_MSC_VER`, `__GNUC__`), and from the compiler's *name* when there
    /// is not and the name is unambiguous — a `cl` that could not be asked is still MSVC.
    pub dialect: Option<Dialect>,
    /// How this toolchain was found, which is the first thing to check when an analysis is wrong.
    pub source: ToolchainSource,
    /// Something a user should know about this answer, when there is something: that the macros are unknown, that
    /// the headers came from a convention rather than from a compiler.
    pub note: Option<String>,
}

/// How a toolchain was found, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainSource {
    /// `compile.compiler` in `.cppls.toml`: a person naming the compiler.
    Configuration,
    /// The compile database's own command for the file.
    CompileDatabase,
    /// `CMAKE_CXX_COMPILER` in the project's `CMakeCache.txt`.
    BuildCache,
    /// `CXX` or `CC`.
    Environment,
    /// The platform's own toolchain: MSVC on Windows.
    PlatformDefault,
    /// A name found on `PATH`.
    Path,
    /// Nothing could be asked: the system's conventional header directories.
    SystemHeaders,
}

impl ToolchainSource {
    /// A phrase for a report, so that a log line can say how the answer was reached.
    pub fn words(self) -> &'static str {
        match self {
            ToolchainSource::Configuration => "named by .cppls.toml",
            ToolchainSource::CompileDatabase => "named by the compile database",
            ToolchainSource::BuildCache => "named by CMake's cache",
            ToolchainSource::Environment => "named by CXX/CC",
            ToolchainSource::PlatformDefault => "the platform's own toolchain",
            ToolchainSource::Path => "found on PATH",
            ToolchainSource::SystemHeaders => "the system's conventional header directories",
        }
    }

    /// Is this answer a *guess* — headers without the compiler's own word on them?
    pub fn is_a_guess(self) -> bool {
        matches!(self, ToolchainSource::SystemHeaders)
    }
}

impl Toolchain {
    /// The same toolchain, with its source recorded.
    fn claiming(mut self, source: ToolchainSource) -> Self {
        self.source = source;
        self
    }

    /// The compiler, as a report spells it — including the case where there is none.
    ///
    /// A method because five callers print it and each of them would otherwise write its own `match` on the
    /// `Option` — and because "no compiler" is a *result* here rather than a missing value: the headers may still
    /// be real (see [`ToolchainSource::SystemHeaders`]).
    pub fn compiler_name(&self) -> String {
        match &self.compiler {
            Some(compiler) => compiler.display().to_string(),
            None => "no compiler — the system's conventional header directories only".to_string(),
        }
    }

    /// The predefined macros, as the map a condition is evaluated against.
    ///
    /// A name with no value (`#define __linux`) maps to `None`, which is what `defined(NAME)` asks about; a name
    /// with one (`#define __cplusplus 202002L`) keeps it, which is what `#if __cplusplus >= 201703L` needs.
    pub fn macros(&self) -> Vec<(&str, Option<&str>)> {
        self.builtin_macros
            .iter()
            .map(|define| (define.name.as_ref(), define.value.as_deref()))
            .collect()
    }

    /// This toolchain's directories as include paths — all of them **system** paths.
    ///
    /// They are what `-isystem` would name, which is why diagnostics inside them are the compiler's business
    /// rather than the project's. The distinction is not cosmetic: it is what a consumer reads to decide whether
    /// an error in a file is worth showing the user.
    pub fn include_paths(&self) -> Vec<IncludePath> {
        self.system_include_paths
            .iter()
            .cloned()
            .map(IncludePath::system)
            .collect()
    }

    /// A configuration with this toolchain's own directories **appended**.
    ///
    /// # Why appended
    ///
    /// Because that is where the compiler searches them. `g++ -I dir` finds `dir` before any of its own
    /// directories, and `-isystem dir` does too — so a project's paths keep the positions the project gave them,
    /// and the toolchain's come last, which is the order a `#include` is actually resolved in.
    ///
    /// # Why it is idempotent
    ///
    /// A directory already in the list is not added again. The first occurrence is the one that decides — a
    /// search stops at the first directory that has the file — so a second occurrence can never win, and a
    /// configuration that grew every time it was prepared would say the same thing in more words. (GCC reaches
    /// the same conclusion out loud: `ignoring duplicate directory …`.)
    pub fn config(&self, base: &CompilerConfig) -> CompilerConfig {
        let mut config = base.clone();

        for directory in &self.system_include_paths {
            let wanted = normalize_path(directory, cfg!(windows));

            let known = config.include_paths.iter().any(|path| {
                normalize_path(&path.directory, cfg!(windows)) == wanted
            });

            if !known {
                config.include_paths.push(IncludePath::system(directory.clone()));
            }
        }

        config
    }
}

/// The directories a compiler listed between its markers.
///
/// # What is read, and what is not
///
/// Only the `#include <…>` block. GCC prints a `#include "…"` block first, and directories there are ones a
/// **quoted** include finds and an angle one does not (`-iquote`); [`CompilerConfig`] has no way to say that, and
/// adding them as system paths would make `#include <vector>` resolve to a file the compiler would not have
/// found. Measured on both compilers here, the quoted block is empty — this invocation passes no `-iquote` — so
/// nothing is being dropped in practice; the rule exists so that a future invocation which passes one does not
/// silently widen the search.
///
/// An entry ending in ` (framework directory)` — what Clang prints on macOS — loses that suffix, because the
/// suffix is Clang annotating its own output and not part of the path.
///
/// # Case is preserved
///
/// The paths are normalized — separators unified, `..` folded — but **not** case-folded, even on Windows.
/// [`normalize_path`]'s flag exists for *comparison* keys, and these are not keys: they are directories that will
/// be opened. Folding case would hand back `c:/program files/…`, which is a spelling that happens to work on a
/// case-insensitive volume and does not on a case-sensitive one, and which a user reading a diagnostic would not
/// recognise as their own directory. Comparison-time folding still happens where comparison happens — see
/// [`Toolchain::config`].
pub fn parse_search_list(output: &str) -> Vec<PathBuf> {
    const OPENS: &str = "#include <...> search starts here:";
    const CLOSES: &str = "End of search list.";

    let mut directories = Vec::new();
    let mut inside = false;

    for line in output.lines() {
        let trimmed = line.trim();

        if !inside {
            // The marker is compared after trimming, because the lines *around* it are indented and a rule that
            // depended on either compiler's indentation would be a rule about one compiler.
            inside = trimmed == OPENS;
            continue;
        }

        if trimmed == CLOSES {
            break;
        }

        if trimmed.is_empty() {
            continue;
        }

        let without_note = trimmed.strip_suffix(" (framework directory)").unwrap_or(trimmed);
        let directory = PathBuf::from(normalize_path(Path::new(without_note), false));

        if directory.as_os_str().is_empty() {
            continue;
        }

        directories.push(directory);
    }

    directories
}

/// The compiler's version line, when it printed one this can recognise.
///
/// Recognised rather than guessed: the line has to *start* with `gcc version` or `clang version`. A looser rule
/// — "the line with `version` in it" — would answer with `Configured with:` for the MinGW build measured here,
/// and a version string used to explain a wrong analysis must not be a plausible-looking wrong one.
pub fn parse_version(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("gcc version") || line.starts_with("clang version"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{Environment, Output, parse_search_list, parse_version};

    /// GCC 15.1.0 (MinGW-w64), trimmed to three entries — the shape is verbatim, including the `..` segments
    /// and the `#include "..."` block that precedes it.
    ///
    /// **The paths are not**: they are anonymised to `C:\tools\mingw64`, because what this test is about is the
    /// *shape* a compiler prints — `COLLECT_GCC`, the `#include "..."` block that must not be read, the `bin/../lib`
    /// that has to be folded, the `ignoring duplicate directory` line in between — and a fixture that spells out the
    /// directory of the machine it was recorded on is a fixture nobody else can run and nobody can tell from a
    /// machine-specific assertion. (See `docs/msvc-notes.md` for why a recorded shape is worth having at all: the
    /// alternative is a parser written against what a compiler is *documented* to print.)
    const GCC: &str = "\
Using built-in specs.
COLLECT_GCC=C:\\tools\\mingw64\\bin\\g++.exe
Target: x86_64-w64-mingw32
gcc version 15.1.0 (x86_64-win32-seh-rev0, Built by MinGW-Builds project) 
COLLECT_GCC_OPTIONS='-E' '-v' '-shared-libgcc' '-mtune=core2' '-march=nocona'
ignoring duplicate directory \"C:/tools/mingw64/lib/gcc/../../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++\"
#include \"...\" search starts here:
#include <...> search starts here:
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/x86_64-w64-mingw32
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/backward
End of search list.
";

    /// Clang 18 on Windows, trimmed to two entries. Note the backslashes and the absence of `..` — the two
    /// compilers print the same *structure* and different *spellings*, which is exactly why the parser keys on
    /// the structure. No `..` to fold, and a directory with a space in it, which is the other shape that breaks a
    /// parser keyed on whitespace.
    const CLANG: &str = "\
clang version 18.1.8
Target: x86_64-pc-windows-msvc
Thread model: posix
#include \"...\" search starts here:
#include <...> search starts here:
 C:\\Program Files\\LLVM\\lib\\clang\\18\\include
 C:\\Program Files\\Microsoft Visual Studio\\18\\Community\\VC\\Tools\\MSVC\\14.51.36231\\include
End of search list.
";

    #[test]
    fn both_compilers_search_lists_are_read_the_same_way() {
        let gcc = parse_search_list(GCC);
        let clang = parse_search_list(CLANG);

        assert_eq!(gcc.len(), 3, "three entries, and nothing from the quoted block");
        assert_eq!(clang.len(), 2);

        // Normalized, which folds the `..` GCC prints and the separators Clang prints.
        assert_eq!(
            gcc[0],
            std::path::Path::new("C:/tools/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++"),
            "the `bin/../lib` in the compiler's own output is folded"
        );
        assert_eq!(
            clang[0],
            std::path::Path::new("C:/Program Files/LLVM/lib/clang/18/include")
        );
    }

    #[test]
    fn the_quoted_block_is_not_read() {
        // A shape the compiler can print and this must not confuse: directories a **quoted** include finds and an
        // angle one does not. Reading them as system paths would make `#include <vector>` resolve to a file the
        // compiler would not have found — a wrong answer rather than a missing one.
        let output = "\
#include \"...\" search starts here:
 /only/for/quotes
#include <...> search starts here:
 /for/angles
End of search list.
";
        assert_eq!(
            parse_search_list(output),
            [std::path::PathBuf::from("/for/angles")]
        );
    }

    #[test]
    fn an_apple_framework_directory_loses_its_note() {
        // Clang on macOS annotates entries it treats as frameworks. The note is the compiler talking about its
        // own output, not part of the path — a directory named `… (framework directory)` does not exist.
        let output = "\
#include <...> search starts here:
 /System/Library/Frameworks (framework directory)
 /usr/include
End of search list.
";
        assert_eq!(
            parse_search_list(output),
            [
                std::path::PathBuf::from("/System/Library/Frameworks"),
                std::path::PathBuf::from("/usr/include")
            ]
        );
    }

    #[test]
    fn output_without_a_search_list_has_no_directories() {
        // What an unrelated program with the compiler's name would print, and what a compiler that failed to
        // start prints. Both mean "this toolchain cannot be asked".
        assert!(parse_search_list("usage: not-a-compiler [options] file").is_empty());
        assert!(
            parse_search_list("#include <...> search starts here:\n").is_empty(),
            "a list that never ends has no entries, rather than everything after it"
        );
    }

    #[test]
    fn the_version_line_is_the_one_the_compiler_printed() {
        assert_eq!(
            parse_version(GCC).as_deref(),
            Some("gcc version 15.1.0 (x86_64-win32-seh-rev0, Built by MinGW-Builds project)")
        );
        assert_eq!(parse_version(CLANG).as_deref(), Some("clang version 18.1.8"));

        // The line a looser rule would answer with — `Configured with:` mentions a version, and it is not the
        // compiler's. A wrong version string is worse than none, because it is used to explain wrong analyses.
        assert_eq!(parse_version(&GCC.replace("gcc version", "Configured with gcc version")), None);
    }

    #[test]
    fn the_output_documents_both_streams() {
        // GCC prints the list to stderr and Clang to stderr too, but a caller that kept only stdout would report
        // "no compiler" for a compiler that answered.
        let output = Output {
            stdout: "from stdout".to_string(),
            stderr: "from stderr".to_string(),
            status: Some(0),
            succeeded: true,
        };
        let combined = output.combined();
        assert!(combined.contains("from stdout") && combined.contains("from stderr"));
    }

    #[test]
    fn the_environment_is_read_as_data() {
        // Not asserted against the process environment — that would be a test of the machine rather than of the
        // code. What matters is that the shape is usable without touching `std::env`, which is what the tests
        // below rely on.
        let environment = Environment {
            cxx: Some(std::path::PathBuf::from("/opt/gcc/bin/g++")),
            cc: None,
            path: vec![std::path::PathBuf::from("/usr/bin")],
            path_extensions: Vec::new(),
        };
        assert_eq!(environment.path.len(), 1);
    }

    // -------------------------------------------------------------------------------------------
    // Which compiler is asked, and what is done with the answer
    //
    // Every test here runs without a compiler installed, which is the point of `CommandRunner` and
    // `Environment` being data: the policy is what is under test, not this machine's toolchain.
    // -------------------------------------------------------------------------------------------

    use super::{
        COMPILER_NAMES, CommandRunner, CompilerConfig, IncludePath, Path, Toolchain,
        ToolchainSource, discover, find_compiler, parse_builtin_macros, search_paths,
    };
    use crate::include::config::{CompileCommand, CompileCommands};
    use crate::file::paths::{MemoryFiles, normalize_path};

    /// The environment a test names explicitly, with no extensions so that a `PATH` entry is a whole file name.
    fn environment() -> Environment {
        Environment {
            cxx: None,
            cc: None,
            path: vec![Path::new("/bin").into(), Path::new("/usr/bin").into()],
            path_extensions: Vec::new(),
        }
    }

    /// A compiler that answers with a fixed search list, and remembers what it was asked.
    struct Answers {
        program: &'static str,
        output: &'static str,
    }

    impl CommandRunner for Answers {
        fn run(
            &self,
            program: &Path,
            arguments: &[&str],
            environment: &[(std::ffi::OsString, std::ffi::OsString)],
        ) -> Option<Output> {
            if program.file_name()?.to_str()? != self.program {
                return None;
            }

            assert!(
                environment.is_empty(),
                "a GNU-like compiler is asked with an empty environment: {environment:?}"
            );

            // The arguments are asserted rather than ignored: `-x c++` is what makes `gcc` list the C++
            // directories, `-dM` is what makes it print its predefined macros, and a discovery that dropped
            // either would answer a different question. `-std=` is asserted where a standard was given.
            assert!(
                arguments.starts_with(&["-dM", "-E", "-v", "-x", "c++"]),
                "the predefined macros and the search list come from one call: {arguments:?}"
            );
            assert!(
                arguments
                    .iter()
                    .all(|argument| *argument == "-dM"
                        || *argument == "-E"
                        || *argument == "-v"
                        || *argument == "-x"
                        || *argument == "c++"
                        || *argument == "-"
                        || argument.starts_with("-std=")),
                "and nothing else is asked for: {arguments:?}"
            );

            Some(Output {
                stdout: String::new(),
                stderr: self.output.to_string(),
                status: Some(0),
                succeeded: true,
            })
        }
    }

    fn command_line(compiler: &str) -> CompileCommands {
        CompileCommands {
            commands: vec![CompileCommand {
                file: "/p/main.cpp".into(),
                directory: None,
                arguments: vec![compiler.to_string(), "-c".to_string()],
            }],
            malformed: 0,
        }
    }

    #[test]
    fn the_compiler_the_compile_database_names_is_the_one_asked() {
        // Step 1 of the order, and the only step that knows *this* file is built by *that* compiler. A project
        // with two toolchains in it is what it exists for.
        let files = MemoryFiles::new()
            .with_file("/usr/bin/g++", "")
            .with_file("/opt/clang/bin/clang++", "");
        let commands = command_line("/opt/clang/bin/clang++");

        let found = find_compiler(&files, Some(&commands), Path::new("/p/main.cpp"), &environment());

        assert_eq!(found, Some(Path::new("/opt/clang/bin/clang++").to_path_buf()));
    }

    #[test]
    fn a_named_compiler_that_is_not_there_falls_through_to_the_environment() {
        // A checked-in `compile_commands.json` names the machine it was generated on. Requiring that path to
        // exist would make the database worse than useless on every other machine, so a name that is not there is
        // passed over rather than reported.
        let files = MemoryFiles::new().with_file("/usr/bin/clang++", "");
        let commands = command_line("/opt/gcc/bin/g++");

        let mut environment = environment();
        environment.cxx = Some(Path::new("/usr/bin/clang++").into());

        let found = find_compiler(&files, Some(&commands), Path::new("/p/main.cpp"), &environment);
        assert_eq!(found, Some(Path::new("/usr/bin/clang++").to_path_buf()));
    }

    #[test]
    fn cxx_beats_cc_and_path() {
        let files = MemoryFiles::new()
            .with_file("/usr/bin/g++", "")
            .with_file("/usr/bin/cc", "");

        let mut environment = environment();
        environment.cc = Some(Path::new("/usr/bin/cc").into());
        environment.cxx = Some(Path::new("/usr/bin/g++").into());

        assert_eq!(
            find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
            Some(Path::new("/usr/bin/g++").to_path_buf()),
            "`CXX` is the project's stated C++ compiler and `CC` is only a fallback"
        );
    }

    #[test]
    fn a_bare_name_is_searched_on_the_path_and_a_path_is_used_where_it_says() {
        // What a shell does with the same string, which is what the two spellings mean.
        let files = MemoryFiles::new()
            .with_file("/bin/g++", "")
            .with_file("/usr/bin/g++", "")
            .with_file("/opt/other/g++", "");

        let mut environment = environment();

        environment.cxx = Some(Path::new("g++").into());
        assert_eq!(
            find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
            Some(Path::new("/bin/g++").to_path_buf()),
            "a bare name is searched on `PATH`, in order"
        );

        environment.cxx = Some(Path::new("/opt/other/g++").into());
        assert_eq!(
            find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
            Some(Path::new("/opt/other/g++").to_path_buf()),
            "a name with a separator in it says where it is"
        );
    }

    #[test]
    fn the_path_is_searched_in_the_order_the_names_are_tried() {
        // Nothing in the environment: the machine's default is whatever is installed, and `g++` is tried before
        // `clang++` because that is the order a person reading `COMPILER_NAMES` expects.
        let files = MemoryFiles::new().with_file("/usr/bin/clang++", "");
        let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment());
        assert_eq!(found, Some(Path::new("/usr/bin/clang++").to_path_buf()));

        let files = MemoryFiles::new()
            .with_file("/usr/bin/clang++", "")
            .with_file("/usr/bin/g++", "");
        let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment());
        assert_eq!(
            found,
            Some(Path::new("/usr/bin/g++").to_path_buf()),
            "the first name on the list that exists, not the first directory that has any of them"
        );

        assert!(COMPILER_NAMES.contains(&"c++"), "the one name POSIX guarantees");
    }

    #[test]
    fn no_compiler_anywhere_is_no_answer_rather_than_an_error() {
        let files = MemoryFiles::new();
        assert_eq!(
            find_compiler(&files, None, Path::new("/p/main.cpp"), &environment()),
            None
        );
    }

    #[test]
    fn windows_tries_the_extensions_in_the_order_pathext_gives() {
        // `g++` next to a file literally named `g++` and a `g++.exe`: Windows runs the `.exe`, so a search that
        // tried the bare name first would find a file the system would not run.
        let files = MemoryFiles::new()
            .with_case_insensitive(true)
            .with_file("/bin/g++", "")
            .with_file("/bin/g++.exe", "");

        let mut environment = environment();
        environment.cxx = Some(Path::new("g++").into());
        environment.path_extensions = vec![".COM".to_string(), ".EXE".to_string()];

        let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment)
            .expect("`g++` is on the path");

        // Compared through the normalizer rather than as a string: the found path is built by joining, so it
        // carries the platform's separator, and the extension keeps the case `PATHEXT` gave it. Both are
        // spellings of one file, which is exactly what `normalize_path` is for.
        assert_eq!(
            normalize_path(&found, true),
            normalize_path(Path::new("/bin/g++.exe"), true),
            "the executable spelling, not the bare name that sits beside it"
        );
    }

    #[test]
    fn a_compiler_that_answers_gives_its_directories_its_version_and_its_macros() {
        let runner = Answers {
            program: "g++",
            output: GCC,
        };

        let toolchain =
            search_paths(&runner, Path::new("/usr/bin/g++"), None).expect("the fixture answers");

        assert_eq!(toolchain.compiler.as_deref(), Some(Path::new("/usr/bin/g++")));
        assert_eq!(toolchain.system_include_paths.len(), 3);
        assert!(toolchain
            .version
            .as_deref()
            .is_some_and(|version| version.starts_with("gcc version")));
        assert!(
            toolchain.include_paths().iter().all(|path| path.is_system),
            "a toolchain's own directories are what `-isystem` would name, which is what decides whether a \
             diagnostic inside them is the project's business"
        );
    }

    #[test]
    fn a_program_that_is_not_a_compiler_is_not_asked_twice() {
        // A file that happens to be named `g++` and prints something else. `None` is the same answer as "could
        // not run", because both mean the same thing to a caller: this toolchain cannot be asked.
        let runner = Answers {
            program: "g++",
            output: "usage: g++ [options] file\n",
        };
        assert_eq!(search_paths(&runner, Path::new("/usr/bin/g++"), None), None);
    }

    #[test]
    fn the_predefined_macros_are_read_out_of_the_same_answer() {
        // `-dM`'s lines arrive in the middle of `-v`'s, so this is the read that has to tell them apart. The three
        // shapes that matter: a value, no value at all, and a function-like macro — whose parameter list is glued to
        // the name in this output and must not become part of it.
        let macros = parse_builtin_macros(
            "#define __cplusplus 201703L\n\
             #define __linux 1\n\
             #define __STDC_HOSTED__ 1\n\
             #define assert(expr) ((expr) ? (void)0 : abort())\n\
             #define __FLT_MIN__ 1.17549435082228750797e-38F\n\
             gcc version 11.2.0\n\
             #include <...> search starts here:\n",
        );

        let value_of = |name: &str| {
            macros
                .iter()
                .find(|define| define.name.as_ref() == name)
                .map(|define| define.value.as_deref())
        };

        assert_eq!(value_of("__cplusplus"), Some(Some("201703L")));
        assert_eq!(value_of("__STDC_HOSTED__"), Some(Some("1")));
        assert_eq!(
            value_of("assert"),
            None,
            "a function-like macro is not a name a condition can test"
        );
        assert_eq!(
            value_of("__FLT_MIN__"),
            Some(Some("1.17549435082228750797e-38F")),
            "a value is everything up to the line's end, floats included"
        );
        assert_eq!(value_of("gcc"), None, "`-v`'s own output is not a macro");
    }

    #[test]
    fn a_standard_the_caller_names_is_passed_to_the_compiler() {
        // The value of `__cplusplus` is the one a condition's answer depends on, and the compiler prints the value
        // of the language version it was **invoked** for. Asking without `-std=` answers for its default, which is a
        // *wrong* number rather than a missing one — so the standard has to travel with the request.
        let runner = Answers {
            program: "g++",
            output: GCC,
        };

        // The fixture's `run` asserts the arguments, so reaching this line at all means `-std=c++20` was passed.
        let toolchain = search_paths(&runner, Path::new("/usr/bin/g++"), Some("c++20"))
            .expect("the fixture answers");
        assert_eq!(toolchain.compiler.as_deref(), Some(Path::new("/usr/bin/g++")));
    }

    #[test]
    fn discovery_walks_from_the_environment_to_the_answer() {
        let files = MemoryFiles::new().with_file("/usr/bin/g++", "");
        let runner = Answers {
            program: "g++",
            output: GCC,
        };

        let toolchain = discover(
            &files,
            &runner,
            None,
            Path::new("/p/main.cpp"),
            &environment(),
            &crate::include::msvc::WindowsLayout::default(),
        )
        .expect("the compiler is on the path and answers");

        assert_eq!(toolchain.compiler.as_deref(), Some(Path::new("/usr/bin/g++")));
        assert_eq!(toolchain.source, ToolchainSource::Path);
        assert!(toolchain.note.is_none());
        assert_eq!(toolchain.system_include_paths.len(), 3);
    }

    #[test]
    fn a_compiler_the_database_names_is_asked_before_the_environment_is() {
        // The order, pinned: the project's own statement about which compiler builds the file comes before the
        // machine's, because a project with two toolchains in it is what the database exists to describe.
        let files = MemoryFiles::new()
            .with_file("/toolchains/bin/g++", "")
            .with_file("/usr/bin/g++", "");
        let runner = Answers {
            program: "g++",
            output: GCC,
        };

        let mut environment = environment();
        environment.path = vec![Path::new("/toolchains/bin").into()];

        let commands = command_line("/toolchains/bin/g++");
        let toolchain = discover(
            &files,
            &runner,
            Some(&commands),
            Path::new("/p/main.cpp"),
            &environment,
            &crate::include::msvc::WindowsLayout::default(),
        )
        .expect("the database's compiler answers");

        assert_eq!(
            toolchain.compiler.as_deref(),
            Some(Path::new("/toolchains/bin/g++"))
        );
        assert_eq!(
            toolchain.source,
            ToolchainSource::CompileDatabase,
            "and the report says which step answered"
        );
    }

    #[test]
    fn a_machine_with_no_compiler_still_has_the_systems_headers_and_no_macros() {
        // The last resort, and the two halves of what it means: `#include <vector>` resolves — so a project is
        // half-usable — and the macro table is empty, so every condition naming `__cplusplus` or `_WIN32` is
        // `Unknown`. The note says so, because a caller that read emptiness as "not defined" would answer wrongly.
        let files = MemoryFiles::new();
        let runner = Answers {
            program: "nothing",
            output: "",
        };
        let mut environment = environment();
        environment.path = Vec::new();
        environment.cxx = None;
        environment.cc = None;

        let found = discover(
            &files,
            &runner,
            None,
            Path::new("/p/main.cpp"),
            &environment,
            &crate::include::msvc::WindowsLayout::default(),
        );

        // A machine with no standard library installed anywhere conventional has no guess at all — which is the
        // honest answer rather than a directory that does not exist — so this test asserts about the answer when
        // there is one.
        if let Some(toolchain) = found {
            assert_eq!(toolchain.source, ToolchainSource::SystemHeaders);
            assert!(toolchain.source.is_a_guess());
            assert_eq!(toolchain.compiler, None);
            assert!(toolchain.builtin_macros.is_empty());
            assert!(
                toolchain
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("Unknown")),
                "the note says what is missing: {:?}",
                toolchain.note
            );
            assert!(
                !toolchain.system_include_paths.is_empty(),
                "and every directory is one that exists"
            );
            for directory in &toolchain.system_include_paths {
                assert!(directory.is_dir(), "{}", directory.display());
            }
        }
    }

    #[test]
    fn the_directories_are_appended_so_the_project_keeps_its_own_order() {
        // A search stops at the first directory that has the file, so position *is* precedence: `-I` finds a
        // header before the compiler's own directories do, which is how a project overrides one.
        let toolchain = Toolchain {
            compiler: Some("/usr/bin/g++".into()),
            version: None,
            system_include_paths: vec!["/gcc/include/c++".into(), "/gcc/include".into()],
            builtin_macros: Vec::new(),
            dialect: None,
            source: ToolchainSource::Path,
            note: None,
        };

        let base = CompilerConfig::new().with_include_path("project/inc");
        let config = toolchain.config(&base);

        assert_eq!(
            config.include_paths,
            [
                IncludePath::user("project/inc"),
                IncludePath::system("/gcc/include/c++"),
                IncludePath::system("/gcc/include"),
            ]
        );
    }

    #[test]
    fn preparing_a_configuration_twice_changes_nothing_the_second_time() {
        // A second occurrence of a directory can never win, so a configuration that grew every time it was
        // prepared would say the same thing in more words — and a caller that prepares it per query would grow it
        // without bound. (GCC reaches the same conclusion out loud: `ignoring duplicate directory …`.)
        let toolchain = Toolchain {
            compiler: Some("/usr/bin/g++".into()),
            version: None,
            system_include_paths: vec!["/gcc/include".into(), "/gcc/include".into()],
            builtin_macros: Vec::new(),
            dialect: None,
            source: ToolchainSource::Path,
            note: None,
        };

        let once = toolchain.config(&CompilerConfig::new());
        let twice = toolchain.config(&once);

        assert_eq!(once, twice);
        assert_eq!(once.include_paths.len(), 1, "and the duplicate inside is folded too");

        let already_there = CompilerConfig::new().with_system_include_path("/gcc/include");
        assert_eq!(
            toolchain.config(&already_there),
            already_there,
            "a directory the project already lists is not added again"
        );
    }
}


