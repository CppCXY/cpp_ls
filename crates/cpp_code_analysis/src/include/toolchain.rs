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
//! records as a change that must bring the macro environment back into the key and bump
//! `FORMAT_VERSION`. calls that the expensive layer and keeps it separate on purpose.
//!
//! # Why the answer is not cached on disk
//!
//! Asking costs **84 ms** (measured, GCC 15.1.0, five runs) — once per session. Writing it down would need a key
//! for "which compiler is this", and a stale include path is a *wrong* answer rather than a slow one: it would
//! make `#include <vector>` resolve into a directory that no longer holds it, and every declaration after it
//! would be attributed to the wrong file. The measured cost does not buy that risk, so the answer lives in memory
//! and is re-asked next session. (: the same reasoning is applied there to the decision
//! *not* to index the standard library ahead of time.)
//!
//! # MSVC is handled by a different question
//!
//! `cl` has no `-v`: its include directories come from `INCLUDE`, which `vcvarsall.bat` sets in the shell that runs
//! the build — an editor started from anywhere else does not have it. So `cl` is asked the way a developer prompt
//! would set it up (`include::msvc`), and this module's `-E -v` path is for everything else. The two meet in
//! [`discover_with`], which is the one place the order between them is decided.
//!
//! # A compiler that is installed is not a compiler on `PATH`
//!
//! Asking `PATH` is asking the *shell*, and an editor started from a menu inherits whichever `PATH` its parent had.
//! Measured on the machine this was written on: clang 18.1.8 sits in `C:\Program Files\LLVM\bin` and is not on
//! `PATH`, so the analysis asked MinGW's `g++` instead — a different standard library, and therefore `std::`
//! answers about declarations this project's build never sees. LLVM's installer writes `PATH` *after* the shell
//! that launched the editor exists, which is why this is a machine state and not a mistake. So the names are also
//! looked for where their installers put them: see `INSTALLED_COMPILER_NAMES`.

use std::path::{Path, PathBuf};

use cpp_parser::Dialect;

use crate::file::paths::{FileProvider, normalize_path};
use crate::include::config::{CommandLineMacro, CompileCommands, CompilerConfig, IncludePath};

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
    /// A caller that read only the text would treat a silent exit 1 as
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
    /// compiler with it ( measures that this works, and that going through `vcvars64.bat`
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
            // exit status and on the `D####`/`C####` codes, as records.)
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

/// The compilers looked for **where an installer puts them**, because `PATH` is not where they are.
///
/// # The measurement this list exists for
///
/// LLVM's Windows installer defaults to `C:\Program Files\LLVM` and offers to add it to `PATH` — an offer that is
/// easy to decline and easy to lose, because it writes the *machine* `PATH` and an editor started from a shell
/// that already existed does not see the change. Measured on the machine this was written on, which has both:
///
/// ```text
/// clang++                                    → (not on PATH)
/// C:\Program Files\LLVM\bin\clang++.exe      → clang version 18.1.8
/// g++                                        → C:\Users\zc\Desktop\mingw\mingw64\bin\g++.exe   ← what was used
/// ```
///
/// So a machine with clang 18 and MSVC 14.51 was analysed against MinGW's libstdc++, which is a *different*
/// standard library — the completion list and every `std::` answer describe declarations this project's build
/// never sees. That is the same failure the platform tier exists to prevent for MSVC, and it has the same fix:
/// look where the installer puts it rather than only where `PATH` says.
///
/// `clang++` first, and `clang++` rather than `clang`, because the `-E -v` search list depends on the driver: a
/// `clang` asked as `-x c++` lists C++'s directories but is configured as a C compiler, so its resource directory
/// and its target differ. The rest of the list is [`COMPILER_NAMES`] in the same order, for the case where a
/// machine has a `g++` in a conventional place and nothing on `PATH` at all.
///
/// **`clang-cl` is not on it**, and `cl` is not either: neither of them can be asked where its headers are. `cl`
/// has no `-E -v` at all, and `clang-cl` has no `-v` *and* no macro dump — so offering either as a discovery
/// candidate would be a process started to learn that it cannot answer. Both are only ever asked when a project
/// **names** one, which is evidence rather than a guess; see [`is_msvc`] for what happens then.
const INSTALLED_COMPILER_NAMES: &[&str] = &["clang++", "g++", "clang", "gcc", "c++"];

/// The directories an installer puts a compiler in, when `PATH` does not say.
///
/// Each entry is a convention of a *distribution*, not a guess: LLVM's installer, Apple's command line tools and
/// Homebrew are where those names live on those machines. None of them is invented, and one that is not there
/// costs a `stat` — see [`installed_compilers`], which is what keeps this list from being an opinion about where
/// software should be installed.
///
/// **Both platforms' lists are returned, on every platform**, which is a deliberate cost of a few stats against a
/// testability win: the Unix entries are absolute paths that a Windows machine simply does not have, and the
/// Windows entries are built from the caller's program files roots, which a Unix caller passes empty. That is what
/// lets `a_compiler_that_is_installed_is_found_when_the_path_does_not_say` — the measurement this whole step
/// exists for — be a test on the machine that recorded it *and* on the Linux CI that runs the same suite.
fn compiler_install_roots(program_files: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    for program_files in program_files {
        // The standalone LLVM installer's default, and Visual Studio's own LLVM component. Which of the two a
        // machine has is not worth two code paths.
        roots.push(program_files.join("LLVM").join("bin"));
        roots.push(
            program_files
                .join("Microsoft Visual Studio")
                .join("LLVM")
                .join("bin"),
        );
    }

    // A hand-unpacked LLVM keeps its version in the directory name (`LLVM-18.1.8-win64`). Newest first, so a
    // machine with two keeps the one a person would have unpacked last.
    let mut versioned: Vec<PathBuf> = Vec::new();
    for program_files in program_files {
        let Ok(entries) = std::fs::read_dir(program_files) else {
            continue;
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.to_ascii_uppercase().starts_with("LLVM") && entry.path().join("bin").is_dir() {
                versioned.push(entry.path().join("bin"));
            }
        }
    }
    versioned.sort_by(|one, two| two.cmp(one));
    roots.extend(versioned);

    // And the filesystem's own conventions, which is what a Unix-like system has instead of an installer.
    roots.extend([
        // Apple's command line tools, and the two places Homebrew puts a formula.
        PathBuf::from("/usr/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        // A compiler unpacked beside its own version number, which is how LLVM's release tarballs are used.
        PathBuf::from("/opt/llvm/bin"),
    ]);

    roots
}

/// Every compiler this machine has in a conventional place, in the order they should be asked.
///
/// A **candidate list**, not a decision: the caller asks each one and keeps the first that answers, for the reason
/// [`discover_with`] gives — a compiler that cannot be asked is skipped rather than fatal.
///
/// The order is by **name** first and directory second, which is the same shape [`COMPILER_NAMES`] has on `PATH`: a
/// `clang++` in the second directory is a better answer than the `g++` in the first, because the question is which
/// compiler this is and not where it is kept. A name that exists in several places yields the first directory's,
/// and the rest are not offered — one installation is one toolchain.
///
/// Nothing here touches the disk except [`compiler_install_roots`], which lists directories: whether a compiler
/// *exists* is asked of the [`FileProvider`], so a test can put a clang in a directory that does not exist and get
/// the answer a real machine would.
fn installed_compilers(
    files: &impl FileProvider,
    environment: &Environment,
    program_files: &[PathBuf],
) -> Vec<PathBuf> {
    // Asked once per call rather than once per candidate: it is a directory listing of a program files root, and
    // the answer cannot change while one discovery runs.
    let roots = compiler_install_roots(program_files);
    let extensions = extensions_for(environment);
    let mut found: Vec<PathBuf> = Vec::new();

    for name in INSTALLED_COMPILER_NAMES {
        for root in &roots {
            let candidate = candidates_in(root, name, &extensions)
                .into_iter()
                .find(|candidate| files.exists(candidate));

            if let Some(candidate) = candidate {
                if !found.contains(&candidate) {
                    found.push(candidate);
                }
                break;
            }
        }
    }

    found
}

/// The extensions to try for a compiler's name in a conventional directory.
///
/// The environment's `PATHEXT` where it has one: a Windows process has it, and it is the platform's own answer to
/// "what does an executable look like here". A caller that built an [`Environment`] by hand — a test — gets the one
/// extension a Windows installer writes, which is what such a machine has. Empty on a system where an executable is
/// a file with the execute bit, and then [`candidates_in`] returns the bare name.
fn extensions_for(environment: &Environment) -> Vec<String> {
    if !environment.path_extensions.is_empty() {
        return environment.path_extensions.clone();
    }

    if cfg!(windows) {
        return vec![".exe".to_string()];
    }

    Vec::new()
}

/// The compiler to ask, or `None` when nothing usable was found.
///
/// # The order, and why it is this one
///
/// ```text
/// 1. the compiler compile_commands.json names for this file  — it is what actually builds the file
/// 2. `CXX`, then `CC`                                        — the project's stated choice
/// 3. `clang++`, `g++`, … where an installer put them         — a compiler this machine has
/// 4. `g++`, `clang++`, `c++`, … on `PATH`                    — a compiler the shell can see
/// ```
///
/// Each step is a weaker claim than the one before it. The build database is the only one that knows *this* file
/// is built by *that* compiler — a project with two toolchains in it is what step 1 exists for — while `PATH` is a
/// guess about the shell and is last because of it.
///
/// **Step 3 before step 4, and that is the whole of what this function changed.** A machine can have both a clang
/// 18 under `C:\Program Files\LLVM` and a MinGW `g++` on `PATH`, and the measured answer used to be the `g++` —
/// a *different* standard library, so every `std::` answer described declarations the project's build never sees.
/// An installation is evidence of what this machine is for; `PATH` is evidence of what one shell was told. So the
/// installed compiler is asked first, and `PATH` is what answers when nothing is installed where installers put
/// things.
///
/// A name with a separator in it is a path and is looked for where it says (`CXX=/opt/gcc/bin/g++`); a bare name
/// is searched on `PATH`, which is what a shell would do with it.
pub fn find_compiler(
    files: &impl FileProvider,
    commands: Option<&CompileCommands>,
    for_file: &Path,
    environment: &Environment,
) -> Option<PathBuf> {
    find_compiler_among(files, commands, for_file, environment, &[])
}

/// [`find_compiler`], with the directories an installer uses passed in.
///
/// The program files roots are the caller's because they are the *platform's* fact — the same one
/// [`WindowsLayout`](crate::include::msvc::WindowsLayout) already carries for `vswhere` — and because a test has
/// to be able to say "a machine whose program files are here" without owning one. `discover` passes the layout's.
pub fn find_compiler_among(
    files: &impl FileProvider,
    commands: Option<&CompileCommands>,
    for_file: &Path,
    environment: &Environment,
    program_files: &[PathBuf],
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

    installed_compilers(files, environment, program_files)
        .into_iter()
        .next()
        .or_else(|| {
            COMPILER_NAMES
                .iter()
                .find_map(|name| locate(files, Path::new(name), environment))
        })
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
/// **The flags that change which directories a compiler searches**, in the order the command wrote them.
///
/// A compile database entry is more than a compiler and a standard: a project built for another target, or against
/// another library, says so in its own command, and those flags decide the search list completely.
///
/// ```text
/// clang++ --target=x86_64-w64-mingw32 --gcc-toolchain=C:/mingw64 -std=c++23   ← a MinGW build
/// ```
///
/// Asked without them, that clang answers with its **default** target's directories — on Windows the MSVC ones — and
/// the analysis then reads a standard library the project's build never sees. Measured on this machine: a project
/// whose command names clang++ for MinGW was configured with cl, `<vector>` resolved to MSVC's, and the include list
/// had none of the eight libstdc++ directories. Only these flags are taken: an `-I` is the project's own search path
/// and travels with the file's configuration, not with the compiler's answer.
///
/// The list is deliberately small and each entry is a *flag family* rather than a spelling this layer invents.
fn search_list_flags(arguments: &[String]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    let mut rest = arguments.iter().skip(1).peekable();

    while let Some(argument) = rest.next() {
        let takes_a_separate_value = matches!(
            argument.as_str(),
            "-target" | "--target" | "-isystem" | "-isysroot" | "-B" | "-stdlib"
        );

        let starts_with = [
            "--target=",
            "--gcc-toolchain=",
            "--sysroot=",
            "-stdlib=",
            "-march=",
            "-mtune=",
        ]
        .iter()
        .any(|prefix| argument.starts_with(prefix));

        if starts_with || takes_a_separate_value {
            taken.push(argument.clone());
            if takes_a_separate_value && let Some(value) = rest.next() {
                taken.push(value.clone());
            }
        }
    }

    taken
}

pub fn search_paths(
    runner: &impl CommandRunner,
    compiler: &Path,
    standard: Option<&str>,
    flags: &[String],
) -> Option<Toolchain> {
    // **What the project stated, or the newest the compiler takes.** See [`ASSUMED_STANDARDS_GNU`]: the last
    // candidate is "no flag at all", so a compiler that knows neither spelling is still asked rather than skipped.
    for candidate in standards_to_try(standard, &ASSUMED_STANDARDS_GNU) {
        if let Some(found) =
            search_paths_with(runner, compiler, candidate, standard.is_none(), flags)
        {
            return Some(found);
        }
    }

    None
}

/// One attempt: ask `compiler` with `standard` (or with no `-std=` at all, for `None`).
fn search_paths_with(
    runner: &impl CommandRunner,
    compiler: &Path,
    standard: Option<&str>,
    assumed: bool,
    flags: &[String],
) -> Option<Toolchain> {
    // `-E` stops after preprocessing, `-v` prints what the driver is doing, `-x c++` says which language's
    // directories to list (without it, `g++` would still list C++'s, but `gcc` would list C's), and `-` reads an
    // empty translation unit from standard input. `-dM` adds the predefined macros to that output.
    //
    // **The project's own search-list flags go with them**, before the language: `--target` and `--gcc-toolchain`
    // are what decide which compiler and which library this project builds against — see [`search_list_flags`].
    let language = standard.map(|standard| format!("-std={standard}"));
    let mut arguments: Vec<&str> = vec!["-dM", "-E", "-v"];
    arguments.extend(flags.iter().map(String::as_str));
    arguments.extend(["-x", "c++"]);
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

    // **Is this CUDA?** — see [`a_cuda_toolchain`].
    let cuda = a_cuda_toolchain(&builtin_macros, Some(compiler));
    Some(Toolchain {
        compiler: Some(compiler.to_path_buf()),
        version: parse_version(&combined),
        system_include_paths,
        builtin_macros,
        cuda,
        dialect,
        standard: standard.map(str::to_string),
        // Overwritten by the caller that knows which step of the order found this compiler.
        source: ToolchainSource::Path,
        note: assumed.then(|| assumed_standard_note(standard)).flatten(),
    })
}

/// What a report says when the analysis chose the standard because nothing else did.
///
/// `None` for the last candidate of the assumed list — the compiler's own default is not an assumption the analysis
/// made, it is the compiler answering the question it was asked, and claiming otherwise would be a note about
/// nothing. See [`ASSUMED_STANDARD_MSVC`] for why the assumed path exists at all.
fn assumed_standard_note(standard: Option<&str>) -> Option<String> {
    let standard = standard?;

    Some(format!(
        "no build configuration states a language standard (no compile_commands.json, no CMakeLists.txt, no \
         .cppls.toml), so the compiler was asked for its newest (`{standard}`); a project that builds as an older \
         standard says so in .cppls.toml"
    ))
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

/// **What the project says about how this file is built** — the two statements a compiler has to be asked about,
/// and the reason they travel together.
///
/// The database is per file and says both the compiler and the flags; `.cppls.toml` (and CMake's cache) may state
/// the language standard with no database at all. They are one value because the question they answer is one
/// question — *what language version is this file built as* — and the compiler's macro table is only useful if it
/// was printed for that version: `__cplusplus` is what every `#if` in every header asks, and a table printed for
/// some other standard is a table that silently says `<format>` is empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuildStatement<'a> {
    /// The compile database, when the project has one.
    pub commands: Option<&'a CompileCommands>,
    /// The language standard the project stated, when the database states none: `-std=c++17` in `.cppls.toml`'s
    /// `[compile] args`, or `CMAKE_CXX_STANDARD` in the build tree.
    pub standard: Option<&'a str>,
}

impl BuildStatement<'_> {
    /// The standard this file is built with, when anything said so.
    ///
    /// The database first, because it is per file: a project with one C++17 target and one C++20 target is what a
    /// compile database exists to describe, and a single statement in `.cppls.toml` cannot express it.
    ///
    /// `read` is the file provider, because the command may name a **response file** — see
    /// [`CompileCommand::to_config_with`] — and CMake writes every target's flags that way.
    fn standard_for(
        &self,
        for_file: &Path,
        read: &mut dyn FnMut(&Path) -> Option<String>,
    ) -> Option<String> {
        self.commands
            .and_then(|commands| commands.command_for(for_file))
            .and_then(|command| command.to_config_with(read).standard)
            .map(|standard| standard.to_string())
            .or_else(|| self.standard.map(str::to_string))
    }
}

/// Find a compiler and ask it. The one call a caller needs.
///
/// # The order, and what each step is worth
///
/// ```text
/// 1. compile_commands.json's compiler for this file   the project saying which compiler builds it
/// 2. `CXX`, then `CC`                                 the person saying which compiler
/// 3. the platform's own: MSVC on Windows the machine's convention
/// 4. `g++`, `clang++`, `c++`, … on `PATH`             the machine's default
/// 5. `clang++`, `g++`, … where an installer put them  the same names, for a `PATH` that does not say
/// 6. the system's header directories                  nothing could be asked: headers without macros
/// ```
///
/// Each step is a weaker claim than the one before it, and the answer says which one it came from
/// ([`Toolchain::source`]) — because "the analysis used MinGW's headers for an MSVC project" is a question that
/// gets asked, and the answer has to be in the report rather than reconstructed from a log.
///
/// **Windows prefers MSVC** unless the project or the person said otherwise: a Windows project's standard library
/// is the one that ships with the toolchain the build uses, and a `g++` on `PATH` (MinGW, say) has a *different*
/// libstdc++ and a different Windows SDK — analysing an MSVC project against it answers about declarations the
/// build never sees. Step 5 is the other half of that: a clang that is installed but not on `PATH` is a *clang*,
/// and answering with MinGW for it is the same wrong answer by a different route.
///
/// `None` when nothing at all could be found: no compiler, and not even a conventional header directory. That is a
/// state the analysis reports (every include stays unresolved) rather than papering over.
pub fn discover(
    files: &impl FileProvider,
    runner: &impl CommandRunner,
    statement: BuildStatement<'_>,
    for_file: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
) -> Option<Toolchain> {
    // The database's own compiler is the one candidate this entry point can work out for itself, and it is the
    // first one: the project saying which compiler builds *this* file. A caller that also knows what `.cppls.toml`
    // and `CMakeCache.txt` say uses [`discover_with`], which takes the whole ordered list — including this one,
    // because it is the caller that knows where it ranks (`.cppls.toml` above it, CMake below).
    let from_database = database_compiler(statement.commands, for_file)
        .map(|compiler| (compiler, ToolchainSource::CompileDatabase));

    discover_with(
        files,
        runner,
        from_database.as_slice(),
        statement,
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
/// 4. `clang++`, `g++`, … where an installer put them, then the names only `PATH` knows
///                                                 what this machine has
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
    statement: BuildStatement<'_>,
    for_file: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
) -> Option<Toolchain> {
    // **The standard the compiler is asked about**: what the project says this file is built as, or — when nobody
    // says — the newest the compiler takes. See [`Toolchain::standard`] for why the answer travels back, and
    // [`ASSUMED_STANDARD_MSVC`] for why the last case is not left to the compiler's own default.
    let standard = statement.standard_for(for_file, &mut |path| files.read(path));

    // **What the project`s own command says about its compiler`s search list** — a target, a GCC toolchain, a
    // sysroot. See [`search_list_flags`] for the measurement that made this necessary.
    let search_flags: Vec<String> = statement
        .commands
        .and_then(|commands| commands.command_for(for_file))
        .map(|command| search_list_flags(&command.arguments))
        .unwrap_or_default();

    // 1. The project's own answers.
    for (name, source) in named {
        if let Some(toolchain) = ask(
            files,
            runner,
            name,
            environment,
            layout,
            standard.as_deref(),
            &search_flags,
            for_file,
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
            &search_flags,
            for_file,
        ) {
            return Some(toolchain.claiming(ToolchainSource::Environment));
        }
    }

    // 3. The platform's own toolchain.
    if let Some(toolchain) = ask_msvc(runner, layout, standard.as_deref(), None, for_file) {
        return Some(toolchain.claiming(ToolchainSource::PlatformDefault));
    }

    // 4. Whatever this machine has: the names where an installer put them, then the names only `PATH` knows, and
    // each name once — a `clang++` on `PATH` and the one in `C:\Program Files\LLVM\bin` are the same installation
    // found twice, and asking a second time is another process for an answer already in hand.
    let mut candidates: Vec<PathBuf> =
        find_compiler_among(files, None, for_file, environment, &layout.program_files)
            .into_iter()
            .collect();

    for name in COMPILER_NAMES {
        if let Some(on_path) = locate(files, Path::new(name), environment)
            && !candidates.contains(&on_path)
        {
            candidates.push(on_path);
        }
    }

    for name in &candidates {
        if let Some(toolchain) = ask(
            files,
            runner,
            name,
            environment,
            layout,
            standard.as_deref(),
            &search_flags,
            for_file,
        ) {
            return Some(toolchain.claiming(ToolchainSource::Path));
        }
    }

    // 5. Nothing to ask: the headers a convention promises, and no macro table at all.
    system_headers_fallback(runner, layout)
}

/// **The language standard assumed when nothing states one** — Microsoft's spelling of "the newest".
///
/// # Why there is a default at all, and why it is the newest
///
/// A project with no `compile_commands.json`, no `CMakeLists.txt` and no `.cppls.toml` states nothing, and the
/// question "what is `__cplusplus` here" then has three candidate answers: the compiler's own default, the newest
/// the compiler takes, or `Unknown`. The compiler's default is the one that reads as an answer while being about
/// nobody's build — measured on a real project (a single `.cpp` and an empty `.vscode/settings.json`): MSVC's
/// default is C++14, so `<format>` was **empty**, `std::format` was not a member of `std`, and a reader typing
/// `#include <format>` got nothing from completion, hover or the signature — while the analysis was, technically,
/// describing that configuration exactly. `Unknown` is worse still: every `#if __cplusplus >= …` in every header
/// would take no branch, which is how a standard library disappears.
///
/// So the default is **the newest standard the compiler will take**, which is what a reader writing `#include
/// <format>` is asking about, and it is a *stated* assumption rather than a silent one: the toolchain carries it
/// ([`Toolchain::standard`]), the report says it ([`Toolchain::note`]), and a project overrides it exactly as it
/// overrides anything else — `[compile] args = ["-std=c++17"]` or `remove_args`.
///
/// `/std:c++latest` rather than `/std:c++23preview`: the first is what MSVC calls "newest" at every version, and a
/// spelling a toolset does not know is a compiler that answers **nothing** — see [`standards_to_try`], which falls
/// back to asking with no flag rather than losing the toolchain.
const ASSUMED_STANDARD_MSVC: &str = "c++latest";

/// The same, for GCC and Clang, in the order to try: `c++23` where it is known, the older `c++2b` spelling where it
/// is not, and then no flag at all.
const ASSUMED_STANDARDS_GNU: [&str; 2] = ["c++23", "c++2b"];

/// The standards to ask a compiler about, in order.
///
/// `Some(one)` when the project stated one — one question, one answer, and a compiler that rejects the spelling is
/// a toolchain that could not be asked, which is the honest outcome. The assumed path ends with **`None`**: asking
/// with no flag is still an answer (an older one), and it is better than skipping the candidate and analysing the
/// project against no compiler at all.
fn standards_to_try<'a>(stated: Option<&'a str>, assumed: &[&'a str]) -> Vec<Option<&'a str>> {
    match stated {
        Some(stated) => vec![Some(stated)],
        None => assumed
            .iter()
            .map(|standard| Some(*standard))
            .chain(std::iter::once(None))
            .collect(),
    }
}

/// Ask one compiler by name, whichever kind it is.
fn ask(
    files: &impl FileProvider,
    runner: &impl CommandRunner,
    name: &Path,
    environment: &Environment,
    layout: &crate::include::msvc::WindowsLayout,
    standard: Option<&str>,
    flags: &[String],
    for_file: &Path,
) -> Option<Toolchain> {
    let found = locate(files, name, environment)?;

    if is_msvc(&found) {
        return ask_msvc(runner, layout, standard, Some(found), for_file);
    }

    search_paths(runner, &found, standard, flags)
}

/// Is this Microsoft's compiler?
///
/// By name, because the two kinds are asked in completely different ways and a compiler's name is the only thing
/// an analysis can know before running it: `cl` has no `-E -v`, and its search list comes from `INCLUDE`. A
/// program called `cl` that is not Microsoft's would fail the macro dump and be reported as a compiler that could
/// not be asked — which is the honest outcome, and not a silent wrong answer.
///
/// # `clang-cl` is not `cl`, and the difference is not cosmetic
///
/// Measured on a machine with LLVM 18 installed: `clang-cl` with `cl`'s own macro-dump flags prints **nothing**
/// and exits `1` — that dump is `cl`'s, and clang's MSVC driver does not implement it. So `clang-cl` stays out of
/// this branch, deliberately, and pays for it: a `clang-cl` a project **names** (`compiler` in `.cppls.toml`, the
/// first argument of a compile database entry) falls through to [`search_paths`], which asks it `cl`'s flags as
/// GNU ones, gets an error, and **skips** the candidate in favour of whatever the machine offers next.
///
/// Answering for it properly needs the MSVC search list *plus* the resource directory clang keeps its own builtin
/// headers in (`lib\clang\18\include`, where `stddef.h` and the `stdarg.h` family live) — one of those without the
/// other is a toolchain that resolves `<vector>` and cannot find `<stddef.h>`, which is worse than saying so.
/// [`Toolchain::source`] reports which candidate answered, so the skip is visible rather than silent.
/// `clang++` — the same compiler, the GNU driver, and what LLVM's Windows installer puts beside `clang-cl` — **is**
/// handled, and is what `INSTALLED_COMPILER_NAMES` looks for first.
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
    for_file: &Path,
) -> Option<Toolchain> {
    if !cfg!(windows) {
        return None;
    }

    let mut msvc = crate::include::msvc::discover(runner, layout)?;
    if let Some(named) = named {
        msvc.toolset.cl = named;
    }

    // **The project's standard, or the newest this compiler takes** — see [`ASSUMED_STANDARD_MSVC`]. The macro
    // table is what carries `_MSVC_LANG` and `__cplusplus`, so the standard has to be settled *before* the compiler
    // is asked: a table printed for the default C++14 is a table that says `<format>` is empty, and no later step
    // can tell that apart from a project that really is C++14.
    let mut asked = None;
    let mut macros = None;
    let mut assumed = false;

    for candidate in standards_to_try(standard, &[ASSUMED_STANDARD_MSVC]) {
        // **The file's own directory is the fallback** for the scratch file `cl` needs: it is the one directory the
        // caller definitely has (the analysis was opened on it), so a process that may not write the system
        // temporary directory can still be told what its compiler predefines. See
        // `msvc::predefined_macros_with` for what an empty table costs.
        let fallbacks: Vec<PathBuf> = for_file
            .parent()
            .map(|directory| vec![directory.to_path_buf()])
            .unwrap_or_default();

        if let Some(found) =
            crate::include::msvc::predefined_macros_with(runner, &msvc, candidate, &fallbacks)
        {
            asked = candidate;
            macros = Some(found);
            assumed = standard.is_none();
            break;
        }
    }

    let (builtin_macros, version, note) = match macros {
        Some(macros) => {
            let version = crate::include::msvc::version_line(&msvc, &macros);
            let note = assumed.then(|| assumed_standard_note(asked)).flatten();
            (macros, Some(version), note)
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

    // **MSVC's toolchain is never CUDA on its own.** A CUDA project on Windows drives `cl` as the host compiler, but
    // the fact that CL was found says nothing about the driver — `nvcc` is what makes a unit CUDA, and it is found
    // from the compile database's command, not from the host compiler the platform happens to have.
    let cuda = a_cuda_toolchain(&builtin_macros, None);
    Some(Toolchain {
        compiler: Some(msvc.toolset.cl.clone()),
        version,
        system_include_paths: msvc.include_paths(),
        builtin_macros,
        cuda,
        // Knowing the compiler is `cl` is knowing the dialect, whether or not it answered: MSVC's reading of
        // `__int128`, of attributes and of the preprocessor's own syntax is what the parser needs to know.
        dialect: Some(Dialect::Msvc),
        standard: asked.map(str::to_string),
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
        cuda: false,
        dialect: None,
        // Nothing was asked, so nothing was assumed: the standard is part of what a compiler's answer carries, and
        // there is no compiler here. A project in this state is one where every condition naming `__cplusplus` is
        // `Unknown`, which the note says.
        standard: None,
        source: ToolchainSource::SystemHeaders,
        note: Some(format!(
            "no compiler could be asked; using the system's own header directories ({}), so compiler macros \
             are Unknown",
            found.based_on
        )),
    })
}

/// **Is this toolchain CUDA?** — `__CUDACC__` in its macro table, or an `nvcc` as the compiler.
///
/// The macro is `nvcc`'s own statement about itself, and the name is the fallback for a compiler that could not be
/// asked: an `nvcc` that answers nothing is still nvcc, which is the rule [`Dialect`] follows for `cl`.
///
/// What this is **not** is "the host compiler is MSVC": on Windows a CUDA project drives `cl.exe` as the host
/// compiler, and finding `cl` says nothing about whether the driver is `nvcc`. The two layers are independent, which
/// is the whole reason [`Toolchain::cuda`] and [`Toolchain::dialect`] are separate fields.
fn a_cuda_toolchain(
    builtin_macros: &[CommandLineMacro],
    compiler: Option<&std::path::Path>,
) -> bool {
    builtin_macros
        .iter()
        .any(|macro_| macro_.name.as_ref() == "__CUDACC__")
        || compiler
            .and_then(|compiler| compiler.file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("nvcc"))
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
    /// **Is this a CUDA toolchain?** — `nvcc`, or a compiler a CUDA project drives.
    ///
    /// CUDA is not a dialect of C++ in the sense [`Dialect`] means: `nvcc` compiles host code by *invoking* a host
    /// compiler (MSVC on Windows, GCC on Linux — `-ccbin` picks it), so a `.cu` file is C++ *and* MSVC-or-GNU *and*
    /// CUDA at once. This flag is the third layer, and it is what the parser's CUDA rules are behind
    /// ([`cpp_parser::ParserConfig::cuda`]): the placement qualifiers and the `<<<…>>>` brackets, which are syntax
    /// neither the language nor either vendor's dialect has.
    ///
    /// Read from the macro table (`__CUDACC__` is nvcc's own statement about itself) and, when there is no table,
    /// from the compiler's *name*: an `nvcc` that could not be asked is still nvcc.
    pub cuda: bool,
    /// The dialect the parser should read the project's files in, when the toolchain settles it.
    ///
    /// From the macro table when there is one (`_MSC_VER`, `__GNUC__`), and from the compiler's *name* when there
    /// is not and the name is unambiguous — a `cl` that could not be asked is still MSVC.
    pub dialect: Option<Dialect>,
    /// **The language standard the compiler was asked about** — what the project stated, or the assumption this
    /// analysis made because nothing stated one.
    ///
    /// It is kept because the answer is not reproducible from anything else: the value of `__cplusplus` in
    /// [`Toolchain::builtin_macros`] is the value for *this* standard, and a reader comparing two runs of the same
    /// project — or asking why `<format>` is empty — needs to see which one it was. [`Toolchain::config`] puts it
    /// into the configuration, where [`crate::include::config::predefined_macros_of`] uses it to state
    /// `__cplusplus`/`_MSVC_LANG` even when the compiler could not be asked. See [`ASSUMED_STANDARD_MSVC`].
    pub standard: Option<String>,
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

    /// **Where this compiler keeps a module the project does not contain, and what would let it find one it does.**
    ///
    /// The sentence an analysis owes a reader who wrote `import std;` and got nothing. "No file in this project
    /// declares module `std`" is true and useless: the file exists on the machine, in a directory only the compiler
    /// knows, and **which directory that is differs per compiler** — so a message that named one of them would be
    /// wrong on two thirds of the machines this runs on.
    ///
    /// # The three, and what is measured behind each
    ///
    /// * **MSVC.** `import std;` is C++23 and `/std:c++latest` ([`ASSUMED_STANDARD_MSVC`] is what this analysis asks
    ///   for when a project states nothing). The *source* of the standard module ships with the toolchain:
    ///   `<VC>/Tools/MSVC/<version>/modules/std.ixx`, and the build has to compile it into an `.ifc` and name it —
    ///   `/reference std=std.ifc`, or `/ifcSearchDir <dir>`. Measured on 14.35.32215: the file is 3 194 bytes, a
    ///   global module fragment with the C headers, then `export module std;` and an `#include` of every standard
    ///   header. There is also `std.compat.ixx` beside it, which re-exports `std` **and** the C library's global
    ///   names.
    /// * **GCC and Clang.** A module has to have been **built** before anything can import it, and where the result
    ///   goes is not a directory this analysis can look in and find the answer: GCC writes `gcm.cache/<module>.gcm`
    ///   beside where the compiler was run, Clang names a prebuilt module file on the command line
    ///   (`-fmodule-file=<module>=<file>`) or looks in `-fprebuilt-module-path=<dir>`. Neither ships the standard
    ///   library as a module, so `import std;` needs a build step that a project has to have, and until it does there
    ///   is no file to read.
    ///
    /// [`Dialect`] has two values and these are three compilers, so GCC and Clang share a sentence — and that is
    /// the honest direction: the fact this layer has is "the compiler is MSVC or it is not", and a sentence that
    /// guessed which of the two it is would be right half the time for no gain over one that names both mechanisms.
    ///
    /// The compiler's *name* is deliberately not in the sentence — the dialect is what decides it, and the reader
    /// already knows which compiler they are using.
    pub fn module_note(&self, module: &str) -> String {
        match self.dialect {
            Some(Dialect::Msvc) => {
                let source = if module == "std" {
                    format!(
                        " Its source ships with the toolchain, at `{}`.",
                        self.standard_module_source("std").display()
                    )
                } else {
                    String::new()
                };

                format!(
                    "no file this project contains declares module `{module}`. A module outside the project is one \
                     only the compiler can see: MSVC needs its interface unit compiled into an `.ifc` and named with \
                     `/reference {module}=<file>.ifc`, or a directory `/ifcSearchDir` names.{source} Until then the \
                     names it exports are unknown rather than absent."
                )
            }
            _ => format!(
                "no file this project contains declares module `{module}`. A module outside the project is one only \
                 the compiler can see, and this one has to have been built before anything can import it — GCC \
                 writes `gcm.cache/{module}.gcm` beside where it is run (`-fmodules-ts`), Clang takes a prebuilt \
                 module file by name (`-fmodule-file={module}=<file>`) or by directory \
                 (`-fprebuilt-module-path=<dir>`). Until then the names it exports are unknown rather than absent."
            ),
        }
    }

    /// **Where this compiler keeps a partition's interface unit, and what would let it find one.**
    ///
    /// The same question as [`Toolchain::module_note`] about a **partition**, which is not a module: `import :area;`
    /// is only legal inside the module that declares `area`, and the file that declares it is a unit of that module
    /// rather than a module of its own. So the note is not "build a module and name it" but "this partition has no
    /// *file*, and the module's interface is compiled against it".
    ///
    /// # What the compiler requires, measured
    ///
    /// `target/build_partitions.bat`, MSVC 14.35.32215, and the direction is the one that surprises:
    ///
    /// ```text
    /// cl /interface /ifcOutput shapes.ifc shapes.cppm                     → C7621: cannot find module partition "area"
    /// cl /interface /ifcOutput shapes-area.ifc shapes-area.cppm           → ok, no /reference needed
    /// cl /interface /ifcOutput shapes.ifc /reference shapes:area=shapes-area.ifc shapes.cppm → ok
    /// ```
    ///
    /// The primary interface unit needs the **partition's** `.ifc`; the partition needs nothing. The name on the
    /// command line is `module:partition` — `shapes:area` — which is the spelling the standard uses for a partition
    /// and is *not* the name of any module.
    ///
    /// GCC and Clang are named rather than described precisely, for the reason [`Toolchain::module_note`] gives:
    /// this layer's fact is "MSVC or not", and the two build a partition into their module file the same way they
    /// build the module itself.
    pub fn partition_note(&self, module: &str, partition: &str) -> String {
        match self.dialect {
            Some(Dialect::Msvc) => format!(
                "no file this project contains declares partition `:{partition}` of module `{module}`. A partition \
                 is not a module — `import :{partition};` is only legal inside `{module}` — and MSVC compiles the \
                 module's interface against the partition's `.ifc`: `/reference {module}:{partition}=<file>.ifc`. \
                 Until then the names the partition exports are unknown rather than absent."
            ),
            _ => format!(
                "no file this project contains declares partition `:{partition}` of module `{module}`. A partition \
                 is not a module — `import :{partition};` is only legal inside `{module}` — and it has to have been \
                 built into the module's own module file first (GCC: `gcm.cache/{module}.gcm`; Clang: \
                 `-fmodule-file={module}:{partition}=<file>`). Until then the names it exports are unknown rather \
                 than absent."
            ),
        }
    }

    /// Where this toolchain ships the **source** of a standard module, if it ships one at all.
    ///
    /// Beside the include directories rather than inside them — `<VC>/Tools/MSVC/<version>/modules/std.ixx` sits in
    /// a `modules` directory that is a **sibling** of `include`, which is why the module search has to look there
    /// (see `ModuleScanner::search_directories`) and why this is not just the first include path plus a name.
    ///
    /// A path, not a promise: it is returned whether or not the file is there, because the answer is about where the
    /// toolchain *keeps* such a thing and a caller that wants to know whether it exists can ask the filesystem.
    pub fn standard_module_source(&self, module: &str) -> PathBuf {
        for directory in &self.system_include_paths {
            if let Some(parent) = directory.parent() {
                let candidate = parent.join("modules").join(format!("{module}.ixx"));
                if candidate.is_file() {
                    return candidate;
                }
            }
        }

        // Nothing on disk: the conventional location, so that the sentence names a directory rather than nothing.
        match self
            .system_include_paths
            .first()
            .and_then(|first| first.parent())
        {
            Some(parent) => parent.join("modules").join(format!("{module}.ixx")),
            None => PathBuf::from("modules").join(format!("{module}.ixx")),
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

        // **The CUDA layer, applied before anything else**, because every parse of every file of the program is what
        // it changes: a project compiled by `nvcc` has `.cu` files and headers written with `__device__` in them, and
        // the grammar has to know before it reads the first one.
        if self.cuda {
            config = config.with_cuda(true);
        }

        // **The standard the compiler was asked about**, when the project did not state one. It is the same
        // question `__cplusplus` answers, and the configuration is where that answer is kept for the layers that
        // never see a toolchain: a condition, the parser's dialect, and the `#if` in a header nobody has read yet.
        if config.standard.is_none()
            && let Some(standard) = &self.standard
        {
            config = config.with_standard(standard.clone());
        }

        for directory in &self.system_include_paths {
            let wanted = normalize_path(directory, cfg!(windows));

            let known = config
                .include_paths
                .iter()
                .any(|path| normalize_path(&path.directory, cfg!(windows)) == wanted);

            if !known {
                config
                    .include_paths
                    .push(IncludePath::system(directory.clone()));
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

        let without_note = trimmed
            .strip_suffix(" (framework directory)")
            .unwrap_or(trimmed);
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
mod tests;
