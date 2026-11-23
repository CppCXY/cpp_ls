//! Where the files are, and what the compiler was told about them.
//!
//! # Why this is configuration and not a guess
//!
//! Every resolution decision downstream depends on these values, and every one of them is invisible in
//! the source text:
//!
//! * `#include <vector>` finds a different file under `-I /opt/gcc/include` than under the default, and
//!   a wrong answer means every declaration in the file has the wrong types;
//! * `#ifdef _WIN32` is decided by the *target*, not by the machine the editor runs on, so analysing a
//!   Windows codebase on Linux with the host's macros reports half of it as not compiled;
//! * `-DNDEBUG` changes which `#ifdef` branches are live, and it is set by the build type, not by
//!   anything in the file.
//!
//! So the configuration is supplied, and [`CompilerConfig::default`] is deliberately *empty* rather than
//! a plausible guess: an empty configuration makes `#if` conditions `Unknown`, which is honest, while a
//! guessed one makes them wrong, which is not.
//!
//! A caller with a build system reads it from the project's [`compile_commands.json`] rather than writing it
//! down twice — see [`parse_compile_commands`], which turns a translation unit's command line into exactly these
//! fields.
//!
//! [`compile_commands.json`]: https://clang.llvm.org/docs/JSONCompilationDatabase.html

use std::path::{Path, PathBuf};

use cpp_parser::Dialect;

/// An include search directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludePath {
    pub directory: PathBuf,
    /// Was this a *system* directory (`-isystem`, or the toolchain's own)?
    ///
    /// It decides which spelling of `#include` finds it first, and — with `-Wsystem-headers` semantics —
    /// whether diagnostics inside it are worth reporting at all.
    pub is_system: bool,
}

impl IncludePath {
    pub fn user(directory: impl Into<PathBuf>) -> Self {
        IncludePath {
            directory: directory.into(),
            is_system: false,
        }
    }

    pub fn system(directory: impl Into<PathBuf>) -> Self {
        IncludePath {
            directory: directory.into(),
            is_system: true,
        }
    }
}

/// A macro the compiler was given on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLineMacro {
    pub name: Box<str>,
    /// `-DFOO` defines `FOO` as `1`, not as nothing: the standard says so, and code that writes
    /// `#if FOO` depends on it.
    pub value: Option<Box<str>>,
}

impl CommandLineMacro {
    /// `-DFOO` — defined as `1`.
    pub fn defined(name: impl Into<Box<str>>) -> Self {
        CommandLineMacro {
            name: name.into(),
            value: None,
        }
    }

    /// `-DFOO=bar` — defined with a value.
    pub fn with_value(name: impl Into<Box<str>>, value: impl Into<Box<str>>) -> Self {
        CommandLineMacro {
            name: name.into(),
            value: Some(value.into()),
        }
    }
}

/// What the compiler was told.
///
/// Cloneable and cheap to compare, because a file's analysis is only valid for the configuration it was
/// computed under: when the configuration changes, everything computed from it is stale. Comparing whole
/// configurations is how that invalidation is decided, so a configuration that cannot be compared cannot
/// be cached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilerConfig {
    /// **The project's compilation database**, when one was found — what each file's own command says.
    ///
    /// Carried here because the compilation is *per file* and the answer cannot be: a database with one entry per
    /// translation unit says something different for each of them, and a project-wide list built from its first
    /// entry is another target's compilation. That is not a simplification but a wrong answer — a header reachable
    /// only from another target's directory resolves, and an include of `"Config.h"` takes whichever `Config.h` the
    /// search order reaches first. [`CompilerConfig::for_file`] is where it is consulted.
    pub database: Option<std::sync::Arc<CompileCommands>>,
    /// Flags `[compile].remove_args` drops, and `[compile].extra_args` adds — kept here because they apply to
    /// **whichever set won**, and with a compilation database there is no single winning set: each file's own
    /// command is filtered by these in [`CompilerConfig::for_file`], in the documented order (removing first,
    /// then appending), so that "drop the database's `-std=c++17`, add `-std=c++20`" means what it says for every
    /// file rather than only for a project-wide answer that no longer exists.
    pub remove_args: Vec<String>,
    /// See [`CompilerConfig::remove_args`].
    pub extra_args: Vec<String>,
    /// **The project's compilation targets**, when its own file describes them — see
    /// [`TargetSection`](crate::project::TargetSection), and [`CompilerConfig::for_file`] for what they decide.
    ///
    /// Carried on the configuration rather than looked up where it is needed, because the places that need it hold
    /// a configuration and a **file** and nothing else.
    pub targets: Vec<crate::project::TargetSection>,

    /// **The libraries this project's targets may depend on** — `[[library]]` in `.cppls.toml`.
    ///
    /// Carried here for the same reason [`CompilerConfig::targets`] is: [`CompilerConfig::for_file`] is given a
    /// configuration and a file and nothing else, and a target's `depends` is what decides whether a library's include
    /// directories are on that file's search path.
    pub libraries: Vec<crate::project::LibrarySection>,

    /// `-I` and `-isystem`, in the order they were given.
    ///
    /// Order is load-bearing: the first directory that holds the file wins, which is how a project
    /// overrides a system header with its own copy of the same name.
    pub include_paths: Vec<IncludePath>,

    /// `-D`, in the order they were given. Later definitions of a name shadow earlier ones.
    pub defines: Vec<CommandLineMacro>,

    /// `-U`, which removes a definition. Applied after the defines that precede it.
    pub undefines: Vec<Box<str>>,

    /// The standard, as spelled on the command line (`c++20`, `gnu++17`).
    ///
    /// `None` means "not told", which is not the same as "the default": a file that uses C++20 features is
    /// only wrong to reject if the configuration says a standard that lacks them.
    pub standard: Option<Box<str>>,

    /// `--target`, or `-m32`/`-m64`.
    ///
    /// Kept separate from the standard because the two answer different questions, and because the target
    /// is what decides the platform macros — which are not predefined here, since the toolchain that
    /// defines them is not the one analysing.
    pub target: Option<Box<str>>,

    /// **Which compiler** the flags above are for, as far as the *parser* has to care.
    ///
    /// The language is the same everywhere; a handful of reserved spellings are not. `__int128` is a type to g++
    /// and a plain name to cl.exe, `__int64` is the other way round, and `_Float16` is GCC's. Reading one of them
    /// the wrong way is not a diagnostic but a **silent wrong tree** — with `__int128` read as a name,
    /// `unsigned __int128 x;` comes out as a declaration of a variable called `__int128` with a macro suffix `x`
    /// — so the parser has to be told which compiler is reading the file.
    ///
    /// Set from the toolchain's own predefined macros (`__GNUC__` / `_MSC_VER`, see
    /// [`Dialect::from_predefined_macros`]) rather than guessed from the flags, and it is part of the summary's
    /// **context hash**: the same text read for two targets is two different summaries, and a cache that confused
    /// them would serve one target's facts to the other.
    pub dialect: Dialect,

    /// **Read this project as CUDA** — see [`cpp_parser::ParserConfig::cuda`], which is the switch the grammar's
    /// CUDA rules are behind.
    ///
    /// Set by the toolchain (`__CUDACC__` in the predefined macros, or an `nvcc` compiler) and by a project that says
    /// so in `.cppls.toml`. Not the same question as [`CompilerConfig::dialect`]: `nvcc` compiles host code with a host
    /// compiler, so a CUDA program is C++ *and* MSVC-or-GNU *and* CUDA — three independent layers, which is why this is
    /// a flag rather than another [`Dialect`] variant.
    pub cuda: bool,

    /// The directory the compilation was run in.
    ///
    /// A relative `-I` and a relative `#include` are both relative to *this*, not to the file or to the
    /// editor's working directory. Without it, a project's own include paths resolve against nothing in
    /// particular.
    pub working_directory: Option<PathBuf>,

    /// **The project root, for the one search that is about where files *are* rather than where a compiler looked.**
    ///
    /// `None` in every caller that has no root to name — the parser's own tests, a snippet with no project — and the
    /// only thing that reads it is [`Self::fuzzy_includes`]'s fallback.
    pub root: Option<PathBuf>,

    /// **May an `#include` be found by matching the tail of its path against the files under [`Self::root`]?**
    ///
    /// Off by default, and the default is the point: this is a **guess**, and a project that has stated how it is
    /// built must never have a guess shadow what it said. See [`crate::include::fuzzy`] for the rule and what it
    /// refuses to match.
    ///
    /// **On is the interesting setting**, because the alternative is a compile database: a Visual Studio solution
    /// cannot export one, and a checkout nobody has configured does not have one, and in both cases the choice is
    /// between a fuzzy match and a closure of fourteen files. Measured on a real CMake project with no database, the
    /// fallback is what turns `#include "Session/StandardIOSession.h"` from an unresolved name into the header.
    pub fuzzy_includes: bool,

    /// **Was the compilation given `-freflection`?** — GCC 16's switch for C++26 reflection.
    ///
    /// Recorded because it is the only thing that decides the **`__cpp_impl_reflection`** feature-test macro, and that
    /// macro is the gate on libstdc++`s own `<meta>`: the header wraps its whole body in
    /// `#if __glibcxx_reflection >= 202603L` (`bits/version.h`), which is false unless `__cpp_impl_reflection` is
    /// defined. Measured on `C:\Users\zc\Desktop\cpp`: without this, `std::meta`, `define_static_array` and every
    /// metafunction had **no fact at all**, so hover and go-to-definition answered nothing in a file that compiles —
    /// the reader sees "the analysis cannot find std::meta", not "reflection is off".
    ///
    /// A flag rather than a [`Dialect`] variant: the same compiler reads the file, with one more feature on.
    pub reflection: bool,
}

impl CompilerConfig {
    /// **This configuration as it applies to one file** — its target's include directories and defines.
    ///
    /// The reason the type carries [`CompilerConfig::targets`] at all. On a real C++ project the compilation is
    /// *per target*, and one project-wide list is not a simplification but a wrong answer: a header reachable only
    /// from another target's directory resolves, and an include of `"Config.h"` takes whichever `Config.h` the
    /// search order happens to reach first.
    ///
    /// Three rules, in the order a reader would expect:
    ///
    /// ```text
    /// 1. the target's directories go **in front**   its own headers win over anything project-wide
    /// 2. `[compile.include]` follows                the older per-directory key, which this makes work
    /// 3. the rest is unchanged                      flags, undefines, target, compiler
    /// ```
    ///
    /// A file no target describes is returned unchanged, and so is a configuration whose root is unknown — the
    /// paths in a configuration file are relative to the root, so without one there is nothing to resolve.
    /// **The language a file is compiled as**, from its own command's `-std=` — the last thing that decides which syntax
    /// the parser may accept, and until now the one thing nobody asked about.
    ///
    /// Order, and why: the file's own command first, because a compilation database is per translation unit and one
    /// target may be C++17 while another is C++20; then the project's `extra_args`, which is a person saying what the
    /// build is; then `LanguageConfig::permissive`, because a parse that has not been told what it is reading must not
    /// refuse the headers it is reading.
    ///
    /// No stored field: a cached copy would go stale the moment the database or `extra_args` changed, and the database is
    /// already the one place the answer comes from.
    /// **Does the compilation database have a command for this file?** — the first question `language_for` asks,
    /// and the one that says whether a file's language was *stated* rather than defaulted.
    ///
    /// A caller that has a stronger statement about one file — the editor's `languageId` for the document the reader
    /// has open — can then tell "nobody configured this" from "the project says C++20", which is the difference
    /// between overriding a default and overriding a fact.
    pub fn is_in_the_database(&self, file: &Path) -> bool {
        self.database
            .as_ref()
            .is_some_and(|database| database.command_for(file).is_some())
    }

    /// **The name `__FILE__` reports for this file** — the spelling its compile command used.
    ///
    /// A compiler answers the *presumed* name: how the file was written on the command line, which
    /// `compile_commands.json` names in its `file` field and `build_database.json` in its `source`
    /// field. [`CompileCommand::file_as_written`] keeps that spelling beside the resolved path this
    /// analysis opens.
    ///
    /// `None` when no command names the file — a header, a file outside the database. The caller then
    /// uses the file's own path, which is what a compiler invoked with that path reports.
    pub fn presumed_name_for(&self, file: &Path) -> Option<&Path> {
        self.database
            .as_ref()
            .and_then(|database| database.command_for(file))
            .map(|command| Path::new(&command.file_as_written))
    }

    pub fn language_for(&self, file: &Path) -> cpp_parser::features::LanguageConfig {
        let from_the_command = self
            .database
            .as_ref()
            .and_then(|database| database.command_for(file))
            .and_then(|command| {
                cpp_parser::features::LanguageConfig::from_command_line(command.arguments.iter())
            });
        // **A project-wide `-std=` for the *other* family is not this file's language.** `[compile] standard` is a
        // setting for the whole project, and the editor's default for it is `c++23` — so a `.c` file with no
        // compilation database was read as **C++23**: measured through the real server on a two-file C fixture,
        // `language_for` answered `family=Some(Cpp) standards=[Cpp23]`, and every C-only construct (`_Static_assert`,
        // `typeof`, `restrict`, `_Generic`) was then either an error or a name. A compiler given `-std=c++23` and a
        // `.c` file does not build it as C++ either: `-std` selects a version *within* the language the file is in.
        //
        // The rule is therefore the extension's, and only between the two *families*: `.c`/`.i` may not be given a
        // C++ standard, `.cpp`/`.cc`/`.cxx`/`.mpp` may not be given a C one, and a `.h` — included by whichever
        // language includes it — keeps whatever the project said.
        let agrees_with_the_file = |language: &cpp_parser::features::LanguageConfig| {
            use cpp_parser::features::Family;
            // **Only for a C file**, and the other direction is deliberately not filtered: a C++ file handed
            // `-std=c11` is a project stating something unusual rather than a default leaking into the wrong family, and
            // `a_c_style_cast_is_not_a_rule_in_c` depends on exactly that comparison. A `.h` is included by whichever
            // language includes it and keeps whatever the project said.
            match file.extension().and_then(|extension| extension.to_str()) {
                Some("c") | Some("i") => language.family() != Some(Family::Cpp),
                _ => true,
            }
        };
        from_the_command
            // **The project's own `-std=`**, which is where `[compile] standard` and a `-std=` inside
            // `[compile] args` end up (see `project_config_from_flags`). It was missing here, so a project that
            // states its language in its own file was read as *not told*: `args = ["-std=gnu11"]` on neovim's
            // `arglist.c` left the family unknown, and every C++-only rule fired on a C file — measured, **14
            // `cstyle-cast` findings** telling a C program to write `static_cast`.
            .or_else(|| {
                self.standard
                    .as_deref()
                    .and_then(cpp_parser::features::LanguageConfig::from_standard_spelling)
                    .filter(agrees_with_the_file)
            })
            .or_else(|| {
                cpp_parser::features::LanguageConfig::from_command_line(self.extra_args.iter())
                    .filter(agrees_with_the_file)
            })
            // **A `.c` file is C.** When nothing above has said what the file is — no database entry, no
            // `[compile] standard`, no `-std=` — the extension is what every compiler decides by, and it is what the
            // *editor* decided too when it opened the file. Without this the family was unknown, `is_c()` answered
            // false, and every C++-only rule fired on C: measured on vim's `charset.c`, `(unsigned)c` in a binary
            // search was underlined as a C-style cast — a diagnostic telling a C program to write `static_cast`,
            // which the language does not have.
            //
            // `.h` is deliberately not here: a header is included by whichever language includes it, and guessing
            // would put the same false positive on the other family.
            .or_else(|| {
                matches!(
                    file.extension().and_then(|extension| extension.to_str()),
                    Some("c") | Some("i")
                )
                .then(cpp_parser::features::LanguageConfig::c)
            })
            .unwrap_or_else(cpp_parser::features::LanguageConfig::permissive)
    }

    pub fn for_file(&self, file: &Path) -> CompilerConfig {
        self.for_file_with(file, &mut |path| std::fs::read_to_string(path).ok())
    }

    /// **[`CompilerConfig::for_file`] with the file reads the caller's**, which is what a test needs and what a
    /// caller holding an unsaved buffer would need.
    ///
    /// The reads are here because a compile command may name a **response file** and keep its
    /// include paths inside it: CMake writes `@CMakeFiles/<target>.dir/includes_CXX.rsp` for the
    /// Makefile and Ninja generators alike, and on a real project *every* entry of its database was
    /// that shape. Expanding it only for the language standard (which is what happened before) left
    /// the per-file include paths of such a project **empty** — measured: `CommandService.cpp`
    /// cannot see `LanguageServer.h` from one directory up, so the include edge is never built,
    /// `LanguageServer`'s 32 member facts are invisible from that file, and every member query that
    /// starts there refuses.
    pub fn for_file_with(
        &self,
        file: &Path,
        read: &mut dyn FnMut(&Path) -> Option<String>,
    ) -> CompilerConfig {
        let Some(root) = self.root.clone() else {
            return self.clone();
        };

        let mut config = self.clone();
        let mut ahead: Vec<IncludePath> = Vec::new();

        // **The command this file's include paths come from.** Its own when the database has one; otherwise the
        // command of a source beside it, because a header has no translation unit of its own — see
        // [`CompileCommands::a_command_for_a_header`] for the measured consequence (eleven headers of one neovim
        // closure could not see `<uv.h>`, and an unresolved include takes the macro environment's completeness with
        // it).
        let own_command =
            self.database
                .as_ref()
                .and_then(|database| match database.command_for(file) {
                    Some(own) => Some((own, false)),
                    None => database
                        .a_command_for_a_header(file)
                        .map(|beside| (beside, true)),
                });
        if let Some((command, borrowed)) = own_command {
            // **The project's own edits apply to this file's command**, in the order the configuration documents:
            // `remove_args` drops, `extra_args` appends. Doing it here rather than once for the project is what
            // keeps the two keys meaning what they say now that the flags are per file.
            let arguments: Vec<String> = command
                .arguments
                .iter()
                .filter(|argument| !self.remove_args.iter().any(|drop| drop == *argument))
                .cloned()
                .chain(self.extra_args.iter().cloned())
                .collect();

            // **The `@response` file is expanded before anything is read out of the command**, because that is
            // where a CMake build keeps the include paths — see [`CompilerConfig::for_file_with`] for the measured
            // cost of not doing it.
            let arguments = expand_response_files(&arguments, command.directory.as_deref(), read);

            // **The include paths, and only those.** They are what decides which header a name resolves to, so they
            // are the ones that must be this file's; the defines stay project-wide because the **cook** seeds every
            // file's macro environment from the project-wide configuration, and a resolver that disagreed with the
            // cooker about `-DFEATURE_ONLY` would answer about a program nobody compiled. Making the defines per
            // file means making the seed per file, which is the next step rather than half of this one.
            //
            // **But the file's command replaces the *project's* directories, never the compiler's.** A compile
            // command does not spell out its own search path: `g++ -std=c++23 -c x.cpp` names no `-I` for `<string>`,
            // because those directories are built into the compiler and printed only when it is asked (`-E -v`).
            // Taking `own.include_paths` alone therefore deleted the standard library for **every** project that has
            // a database — measured on a four-entry C++23 demo whose commands have no `-I` at all: `<string>`,
            // `<print>`, `<expected>` and `<windows.h>` were all reported missing, and `std::expected<double,
            // std::string>` had no type. The system entries are the one part of the search path the entry knows
            // nothing about, so they survive it — after its own directories, which is the order a compiler searches.
            let own = config_from_arguments(&arguments, command.directory.clone());
            if borrowed {
                // **Added in front rather than instead.** A command of its own is a statement about this file; one
                // borrowed from the source beside it is a guess about whose flags apply, so the project's own
                // directories survive and the borrowed ones go in front — the order a compiler searches.
                let mut merged = own.include_paths;
                for path in config.include_paths.iter() {
                    if !merged.iter().any(|held| held.directory == path.directory) {
                        merged.push(path.clone());
                    }
                }
                config.include_paths = merged;
            } else {
                let system: Vec<IncludePath> = config
                    .include_paths
                    .iter()
                    .filter(|path| path.is_system)
                    .cloned()
                    .collect();
                config.include_paths = own.include_paths;
                for path in system {
                    let known = config
                        .include_paths
                        .iter()
                        .any(|held| held.directory == path.directory);
                    if !known {
                        config.include_paths.push(path);
                    }
                }
            }
        }

        // **Every target that describes the file, nearest first.** `[compile].include` has always documented itself
        // as additive — "*both can be true at once, and the nearer rule is the one that should be added first*" —
        // and a target is the same idea with more to say, so the two are one mechanism here rather than two rules
        // that would disagree. Patterns of different targets usually exclude each other (`Server/**` and `Core/**`),
        // and where they do not, the nearer one's directories come first, which is what "nearest first" means for a
        // search path.
        let mut describing: Vec<(
            usize,
            std::cmp::Reverse<usize>,
            &crate::project::TargetSection,
        )> = self
            .targets
            .iter()
            .enumerate()
            .filter_map(|(order, target)| {
                target
                    .describes(&root, file)
                    .map(|specific| (specific, std::cmp::Reverse(order), target))
            })
            .collect();
        describing.sort_by_key(|(specific, order, _)| (std::cmp::Reverse(*specific), *order));

        for (_, _, target) in describing {
            // **A dependency's include directories come with the target's own** — and nearer first, for the same
            // reason: a file that says its target depends on `widgets` finds `widgets/widget.h` where that library
            // puts it, and a target that names no dependency does not (the honest answer, not an omission).
            for dependency in &target.depends {
                let Some(library) = self
                    .libraries
                    .iter()
                    .find(|library| &library.name == dependency)
                else {
                    continue;
                };
                for directory in library.include_directories(&root) {
                    if !config
                        .include_paths
                        .iter()
                        .any(|held| held.directory == directory)
                        && !ahead.iter().any(|held| held.directory == directory)
                    {
                        ahead.push(IncludePath::user(&directory));
                    }
                }
                for definition in &library.defines {
                    if !config.defines.iter().any(|held| {
                        held.name.as_ref() == definition.split('=').next().unwrap_or("").trim()
                    }) {
                        config.defines.push(define_from_a_config_file(definition));
                    }
                }
            }
            for directory in &target.includes {
                let joined = root.join(directory);
                if !config
                    .include_paths
                    .iter()
                    .any(|held| held.directory == joined)
                    && !ahead.iter().any(|held| held.directory == joined)
                {
                    ahead.push(IncludePath::user(&joined));
                }
            }
            for definition in &target.defines {
                if !config.defines.iter().any(|held| {
                    held.name.as_ref() == definition.split('=').next().unwrap_or("").trim()
                }) {
                    config.defines.push(define_from_a_config_file(definition));
                }
            }
            // The nearest target that states a standard is the one that decides it.
            if config.standard.is_none()
                && let Some(standard) = &target.standard
            {
                config.standard = Some(standard.as_str().into());
            }

            // **A target's whole command line**, when the configuration writes one out. It is the same shape the
            // project's `[compile].args` has, and read in the same order — the arguments it states, its `remove_args`
            // dropped, its `extra_args` appended — but it belongs to the files this target describes rather than to
            // every file. This was accepted by `TargetSection` and never applied, which is the worst of both: a
            // configuration that parses and changes nothing (`docs/worklog.md` P2-7).
            if let Some(declared) = &target.args {
                let arguments: Vec<String> = declared
                    .iter()
                    .filter(|argument| !target.remove_args.iter().any(|drop| drop == *argument))
                    .cloned()
                    .chain(target.extra_args.iter().cloned())
                    .collect();

                // Relative `-I`s are relative to the project root, like the target's own `includes`.
                let own = config_from_arguments(&arguments, Some(root.clone()));

                // A whole command line is a statement about these files, so its directories **replace** what was
                // discovered for them — that is the difference from `includes`, which is additive by design. A relative
                // one is resolved against the project root, the way the target's own `includes` are, so that a stored
                // search path is a path rather than a string whose meaning depends on who reads it.
                config.include_paths = own
                    .include_paths
                    .into_iter()
                    .map(|mut path| {
                        if path.directory.is_relative() {
                            path.directory = root.join(&path.directory);
                        }
                        path
                    })
                    .collect();
                for definition in own.defines {
                    if !config
                        .defines
                        .iter()
                        .any(|held| held.name == definition.name)
                    {
                        config.defines.push(definition);
                    }
                }
                if own.standard.is_some() {
                    config.standard = own.standard;
                }
            }
        }

        // **A file with no command of its own inherits the project's standard, when the project states exactly one.**
        //
        // A header has no translation unit of its own, and the standard is what decides which half of every guarded
        // header exists. Measured on a `<ranges>` fixture whose only database entry is `... /std:c++latest ...` for
        // `main.cpp`: `<ranges>`'s config had no command, so no standard, so `_HAS_CXX20` was false and the cooked
        // rendering of the header was the **12-byte** `#pragma once` stub — `std::views` did not exist and every
        // pipeline answered `UnknownType`. The same rule answers `None` for a project whose entries disagree, where
        // a header really is built two ways.
        if config.standard.is_none()
            && let Some(standard) = self
                .database
                .as_ref()
                .and_then(|database| database.a_unanimous_standard())
        {
            config.standard = Some(standard.into());
        }

        ahead.extend(config.include_paths);
        config.include_paths = ahead;
        config
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_include_path(mut self, directory: impl Into<PathBuf>) -> Self {
        self.include_paths.push(IncludePath::user(directory));
        self
    }

    pub fn with_system_include_path(mut self, directory: impl Into<PathBuf>) -> Self {
        self.include_paths.push(IncludePath::system(directory));
        self
    }

    pub fn with_define(mut self, definition: CommandLineMacro) -> Self {
        self.defines.push(definition);
        self
    }

    pub fn with_standard(mut self, standard: impl Into<Box<str>>) -> Self {
        self.standard = Some(standard.into());
        self
    }

    /// Compile for a **target**: the triple (`x86_64-w64-mingw32`) or the flag (`-m32`) the command line named.
    ///
    /// It decides the platform macros a condition may ask about — see [`predefined_macros_of`], which is where the
    /// names a triple fixes are turned into definitions.
    pub fn with_target(mut self, target: impl Into<Box<str>>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Parse for a **target compiler**: what its reserved spellings mean. See [`CompilerConfig::dialect`].
    pub fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// The dialect in force. [`Dialect::default`] until a toolchain says otherwise.
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// **Read this project as CUDA** — see [`CompilerConfig::cuda`].
    pub fn with_cuda(mut self, cuda: bool) -> Self {
        self.cuda = cuda;
        self
    }

    /// Is this project CUDA? — see [`CompilerConfig::cuda`].
    pub fn cuda(&self) -> bool {
        self.cuda
    }

    pub fn with_working_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(directory.into());
        self
    }

    /// Is the configuration empty — nothing supplied at all?
    ///
    /// Worth asking, because an empty configuration is the state in which conditions are `Unknown` and
    /// includes resolve only relative to the including file. A consumer that wants to warn "this project
    /// has no compile database" asks this rather than inspecting five fields.
    pub fn is_empty(&self) -> bool {
        self.include_paths.is_empty()
            && self.defines.is_empty()
            && self.undefines.is_empty()
            && self.standard.is_none()
            && self.target.is_none()
            && self.working_directory.is_none()
    }

    /// The user (`-I`) directories, in order.
    pub fn user_include_paths(&self) -> impl Iterator<Item = &Path> {
        self.include_paths
            .iter()
            .filter(|path| !path.is_system)
            .map(|path| path.directory.as_path())
    }

    /// The system (`-isystem`) directories, in order.
    pub fn system_include_paths(&self) -> impl Iterator<Item = &Path> {
        self.include_paths
            .iter()
            .filter(|path| path.is_system)
            .map(|path| path.directory.as_path())
    }

    /// Resolve a path that came from the command line against the working directory.
    ///
    /// A relative `-I` is relative to where the compiler ran. A caller that has not said where that was
    /// gets the path unchanged, which is the least surprising answer: it is what the command line said.
    pub fn resolve_against_working_directory(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }

        match &self.working_directory {
            Some(directory) => directory.join(path),
            None => path.to_path_buf(),
        }
    }

    /// Every include directory, in the order a `#include "..."` searches them.
    ///
    /// The including file's own directory is *not* here: it is not a property of the configuration but of
    /// the file doing the including, so it is prepended by the resolver.
    pub fn search_order(&self) -> impl Iterator<Item = (&Path, bool)> {
        self.include_paths
            .iter()
            .map(|path| (path.directory.as_path(), path.is_system))
    }
}

/// One entry of a `compile_commands.json`.
///
/// The format every C++ build system can emit, and the only reliable way to learn how a project is meant
/// to be compiled. Guessing include paths from a directory listing is what produces an analysis that
/// works on the author's machine and fails everywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileCommand {
    /// The file this command compiles, resolved against `directory` — what every reader of the project
    /// uses to name the file.
    pub file: PathBuf,
    /// **The spelling the database wrote**, before [`CompileCommand::file`] resolved it.
    ///
    /// This is the name a compiler would report for `__FILE__`: the *presumed* name is how the file was
    /// spelled on the command line, not the path this analysis later opens. `compile_commands.json`
    /// usually writes it relative to `directory` (or absolute); `build_database.json` writes `source` the
    /// same way. Keeping both is what lets the cook answer `__FILE__` with the command's spelling while
    /// the index keeps one identity per file.
    pub file_as_written: String,
    /// The directory the command runs in.
    pub directory: Option<PathBuf>,
    /// The command, split into arguments.
    pub arguments: Vec<String>,
}

impl CompileCommand {
    /// Read the compiler configuration out of this command's arguments.
    ///
    /// Only the flags that change *what the code means* are read: include paths, defines, the standard,
    /// the target, and the `-o`/`-c`/input/output plumbing that is skipped. A flag this does not know is
    /// ignored rather than guessed at, because a wrong `-D` is worse than a missing one.
    pub fn to_config(&self) -> CompilerConfig {
        config_from_arguments(&self.arguments, self.directory.clone())
    }

    /// The same, with the command's **response files read** — which is what a build system's own command line means,
    /// and what CMake's database needs.
    ///
    /// CMake does not write its include paths into the arguments: it writes
    /// `@CMakeFiles/<target>.dir/includes_CXX.rsp` and puts the `-I` flags in that file, for the Makefile and Ninja
    /// generators alike. Measured on a real CMake project: **every** entry of its `compile_commands.json` was of that
    /// shape, so a reader that does not follow the indirection sees a project with no include paths at all — its own
    /// headers stop resolving, and the file list stops being the build's.
    ///
    /// `read` is the caller's, because this crate reads through a provider rather than the filesystem: the server has
    /// to answer for a buffer nobody has saved, and a test has to be able to say what a file holds.
    pub fn to_config_with(&self, read: &mut dyn FnMut(&Path) -> Option<String>) -> CompilerConfig {
        let arguments = expand_response_files(&self.arguments, self.directory.as_deref(), read);

        config_from_arguments(&arguments, self.directory.clone())
    }
}

/// **The arguments a command really has, once the response files it names have been read.**
///
/// An argument beginning `@` names a file whose contents are arguments — the form CMake uses for the include paths of
/// every target it generates a Makefile or Ninja build for. A relative name is relative to the command's **working
/// directory**, which is what the compiler resolves it against, and the expansion is recursive because a response
/// file may name another. A file that cannot be read is **left as it stands** rather than dropped: the argument is
/// what the build wrote, and an argument this layer cannot interpret is not one it may silently delete.
pub fn expand_response_files(
    arguments: &[String],
    working_directory: Option<&Path>,
    read: &mut dyn FnMut(&Path) -> Option<String>,
) -> Vec<String> {
    /// How deep a chain of response files may go before the rest is left alone. A build system writes one level; the
    /// bound is here so that a file naming itself terminates rather than hanging the analysis.
    const DEEPEST: usize = 8;

    let mut expanded = Vec::with_capacity(arguments.len());
    let mut pending: Vec<(String, usize)> = arguments
        .iter()
        .rev()
        .map(|argument| (argument.clone(), 0usize))
        .collect();

    while let Some((argument, depth)) = pending.pop() {
        let Some(name) = argument.strip_prefix('@').filter(|name| !name.is_empty()) else {
            expanded.push(argument);
            continue;
        };

        let named = Path::new(name);
        let resolved = match (named.is_absolute(), working_directory) {
            (true, _) | (false, None) => named.to_path_buf(),
            (false, Some(directory)) => directory.join(named),
        };

        let Some(text) = read(&resolved).filter(|_| depth < DEEPEST) else {
            expanded.push(argument);
            continue;
        };

        // Reversed, because the stack pops from the end: the file's own arguments keep their order.
        for inner in split_command_line(&text).into_iter().rev() {
            pending.push((inner, depth + 1));
        }
    }

    expanded
}

/// Read a compiler configuration out of a list of arguments.
///
/// The one implementation of "what does this flag mean", used by both sources of flags a project has: the
/// `compile_commands.json` entries the build system wrote, and the `[compile].args` a person wrote in
/// `.cppls.toml`. Two readers of the same flag vocabulary would be free to disagree about `-DFOO=` or `-m64`, and
/// the disagreement would show up as a project that analyses differently depending on which source won.
pub fn config_from_arguments(
    arguments: &[String],
    working_directory: Option<PathBuf>,
) -> CompilerConfig {
    let mut config = CompilerConfig {
        working_directory,
        ..CompilerConfig::default()
    };

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        index += 1;

        // The two spellings of every option: joined (`-Ifoo`) and separate (`-I foo`).
        let take_value = |index: &mut usize| -> Option<&str> {
            let value = arguments.get(*index).map(String::as_str);
            if value.is_some() {
                *index += 1;
            }
            value
        };

        if let Some(rest) = argument.strip_prefix("-isystem") {
            let directory = if rest.is_empty() {
                take_value(&mut index)
            } else {
                Some(rest)
            };
            if let Some(directory) = directory {
                config.include_paths.push(IncludePath::system(directory));
            }
        } else if let Some(rest) = argument.strip_prefix("-I") {
            let directory = if rest.is_empty() {
                take_value(&mut index)
            } else {
                Some(rest)
            };
            if let Some(directory) = directory {
                config.include_paths.push(IncludePath::user(directory));
            }
        } else if let Some(rest) = argument.strip_prefix("-D") {
            let definition = if rest.is_empty() {
                take_value(&mut index)
            } else {
                Some(rest)
            };
            if let Some(definition) = definition {
                config.defines.push(parse_define_argument(definition));
            }
        } else if let Some(rest) = argument.strip_prefix("-U") {
            let name = if rest.is_empty() {
                take_value(&mut index)
            } else {
                Some(rest)
            };
            if let Some(name) = name {
                config.undefines.push(name.into());
            }
        } else if let Some(rest) = argument.strip_prefix("-std=") {
            config.standard = Some(rest.into());
        } else if let Some(directory) = an_msvc_path_argument(argument, "/I", &mut index, arguments)
        {
            config.include_paths.push(IncludePath::user(directory));
        } else if let Some(definition) =
            an_msvc_path_argument(argument, "/D", &mut index, arguments)
        {
            config.defines.push(parse_define_argument(definition));
        } else if let Some(name) = an_msvc_path_argument(argument, "/U", &mut index, arguments) {
            config.undefines.push(name.into());
        } else if let Some(rest) = argument
            .strip_prefix("/std:")
            .or_else(|| argument.strip_prefix("-std:"))
        {
            // **MSVC writes the standard with a slash and a colon** — `/std:c++latest`, `/std:c++20` — and this
            // reader only knew `-std=`, so a project whose *only* statement of its standard is the compile database
            // (every CMake MSVC build, and every `cl.exe` command line) was analysed as the compiler's default.
            // Measured on a `<ranges>` fixture with `... /std:c++latest ...` in its `compile_commands.json`: the
            // header's whole body is inside `#if _HAS_CXX20`, the standard never reached `__cplusplus`, and the
            // cooked rendering of `<ranges>` was the **206-byte** `STL4038` fallback — `std::views` did not exist in
            // the program the analysis read, so every range pipeline answered `UnknownType`.
            config.standard = Some(rest.into());
        } else if argument == "/Zc:__cplusplus" {
            // The switch that makes MSVC report a true `__cplusplus` at all. This layer always defines the macro
            // from the standard, so there is nothing to do beyond not mistaking the spelling for a source file.
            continue;
        } else if argument == "-freflection" {
            // **GCC 16`s reflection switch**, and the one flag this layer needs for `<meta>`: see
            // [`CompilerConfig::reflection`].
            config.reflection = true;
        } else if argument == "-fno-reflection" {
            config.reflection = false;
        } else if let Some(rest) = argument.strip_prefix("--target=") {
            config.target = Some(rest.into());
        } else if argument == "--target" {
            if let Some(target) = take_value(&mut index) {
                config.target = Some(target.into());
            }
        } else if argument == "-m32" || argument == "-m64" {
            config.target = Some(argument.into());
        }
    }

    config
}

/// Apply a project's `[compile]` section to a list of flags and read the result.
///
/// The order is the whole meaning of the three keys, and it is one pass:
///
/// ```text
/// 1. remove_args drops flags from the list that won       (`-Werror`, a stale `-std=c++17`)
/// 2. extra_args appends the project's own                 (`-DFEATURE=1`)
/// 3. the result is read once, left to right               so a later `-std=` wins over an earlier one
/// ```
///
/// Removing **before** appending is what makes "drop the database's `-std=c++17`, add `-std=c++20`" work: the
/// other order would leave the added flag first and have the removed one overwrite it. Matching is by the whole
/// spelling — `-std=c++17` — because that is what a person reads in the database and copies into the file; it is
/// not a pattern language.
pub fn project_config_from_flags(
    flags: &[String],
    working_directory: Option<PathBuf>,
    section: &crate::project::CompileSection,
) -> CompilerConfig {
    let mut arguments: Vec<String> = flags
        .iter()
        .filter(|argument| !section.remove_args.iter().any(|drop| drop == *argument))
        .cloned()
        .collect();

    arguments.extend(section.extra_args.iter().cloned());

    config_from_arguments(&arguments, working_directory)
}

/// Read `-DFOO` or `-DFOO=bar`.
///
/// `-DFOO` defines `FOO` as `1` — the standard says so, and `#if FOO` depends on it. An empty value
/// (`-DFOO=`) is *not* the same thing: it is a definition with an empty body, which `#if FOO` rejects.
/// **MSVC's joined and separated spellings of `-I`, `-D` and `-U`** — `/Ipath`, `/I path`, `/DNAME=value`, `/UNAME`.
///
/// `cl.exe` commands are what an MSVC compile database holds, and this reader knew only the GNU spellings, so a Windows
/// project lost its include directories **and** its feature macros: conditionals chose the wrong branch, generated
/// config headers were not found, and the include closure was a fraction of the program
/// (`docs/worklog.md` P1-10).
///
/// The supported set is written down rather than inferred, because `/D` is also the beginning of ordinary flags:
///
/// ```text
/// /Ipath  /I path        an include directory            supported
/// /DNAME  /D NAME=value  a definition                    supported
/// /UNAME  /U NAME        an undefinition                 supported
/// /I-                    "ignore all include paths"      refused: not a directory
/// /DEBUG /D1 /D2 /doc    ordinary flags that begin /D    refused by name
/// /external:Ipath        a *system* include directory    refused: a different search path, not this one
/// ```
///
/// The refusals are what keep this from inventing a definition called `EBUG` out of `/DEBUG`, which would then change
/// the branches the file is read in — a silently wrong answer in place of a missing one.
fn an_msvc_path_argument<'a>(
    argument: &'a str,
    option: &str,
    index: &mut usize,
    arguments: &'a [String],
) -> Option<&'a str> {
    /// The spellings that begin like one of the three options and are **not** them. Compared against the whole
    /// argument, and case-sensitively, because that is how cl.exe names its flags: /DEBUG is the linker's and
    /// /doc the documentation switch, both ordinary options, while /D is a definition.
    const NOT_THIS_OPTION: [&str; 7] = [
        "/DEBUG",
        "/D0",
        "/D1",
        "/D2",
        "/doc",
        "/INCREMENTAL",
        "/IGNOREIDL",
    ];

    if NOT_THIS_OPTION.contains(&argument) {
        return None;
    }
    let rest = argument.strip_prefix(option)?;
    // /I- is "ignore all include paths", not a directory.
    if rest.starts_with('-') {
        return None;
    }
    let rest = if rest.is_empty() {
        // The separated form: the value is the next argument.
        let value = arguments.get(*index).map(String::as_str)?;
        *index += 1;
        value
    } else {
        rest
    };
    (!rest.is_empty()).then_some(rest)
}

fn parse_define_argument(argument: &str) -> CommandLineMacro {
    match argument.split_once('=') {
        Some((name, value)) => CommandLineMacro::with_value(name, value),
        None => CommandLineMacro::defined(argument),
    }
}

/// Read a `compile_commands.json`.
///
/// A hand-written reader rather than a `serde` dependency, for two reasons: the shape is one array of
/// objects with three string fields, and — the deciding one — a compile database that does not parse must
/// **not** be a hard failure. Projects emit them with comments, with trailing commas, and with entries for
/// languages this analysis has nothing to say about; the useful behaviour is to take the entries that do
/// parse and say how many did not.
pub fn parse_compile_commands(json: &str) -> CompileCommands {
    let mut commands = Vec::new();
    let mut malformed = 0usize;

    for object in top_level_objects(json) {
        match parse_command_object(&object) {
            Some(command) => commands.push(command),
            None => malformed += 1,
        }
    }

    CompileCommands {
        commands,
        malformed,
    }
}

/// **A build database, read for the two fields this layer needs** — CMake's `build_database.json`
/// (`CMAKE_EXPORT_BUILD_DATABASE`, CMake 3.31+, written by the Ninja generators).
///
/// ```json
/// { "version": 1, "revision": 0,
///   "sets": [ { "family-name": "export_build_database", "name": "export_build_database@Debug",
///     "translation-units": [ {
///         "arguments": ["/usr/bin/c++", "-I/…", "-o", "CMakeFiles/app.dir/a.cpp.o", "-c", "/src/a.cpp"],
///         "baseline-arguments": ["…"], "local-arguments": ["…"],
///         "object": "CMakeFiles/app.dir/a.cpp.o",
///         "provides": { "importable": "path/to/bmi" }, "requires": [],
///         "source": "/src/a.cpp", "work-directory": "/build" } ],
///     "visible-sets": [] } ] }
/// ```
///
/// It is a **superset of `compile_commands.json` for the question this layer asks**: each unit carries its source file
/// and its arguments, exactly as the compiler ran them, plus the working directory those arguments are relative to.
/// What it adds — `provides`/`requires`, the module BMIs, `visible-sets` — is C++ modules metadata, which no reader
/// here consumes yet; what it deliberately does not carry is a per-*header* entry (`compile_commands.json` does not
/// either, and [`CompileCommands::a_command_for_a_header`] is how either is used for one).
///
/// `arguments` is optional: when a set shares flags, the command is `baseline-arguments` followed by
/// `local-arguments`, and the two are concatenated here — that is the whole of the split's meaning.
pub fn parse_build_database(json: &str) -> CompileCommands {
    let mut commands = Vec::new();
    let mut malformed = 0usize;

    for object in objects_in_every_array_named(json, "translation-units") {
        match parse_translation_unit(&object) {
            Some(command) => commands.push(command),
            None => malformed += 1,
        }
    }

    CompileCommands {
        commands,
        malformed,
    }
}

/// Read one `translation-units` entry: its source, its working directory, and its command.
fn parse_translation_unit(object: &str) -> Option<CompileCommand> {
    let source = string_field(object, "source")?;
    let directory = string_field(object, "work-directory").map(PathBuf::from);
    let arguments = match array_field(object, "arguments") {
        Some(arguments) if !arguments.is_empty() => arguments,
        _ => {
            let mut arguments = array_field(object, "baseline-arguments").unwrap_or_default();
            arguments.extend(array_field(object, "local-arguments").unwrap_or_default());
            arguments
        }
    };
    if arguments.is_empty() {
        return None;
    }

    // **`source` is relative to `work-directory`** — that is how CMake writes it — so a relative one is resolved here
    // rather than left for every later reader to remember. A reader that does not resolve it asks
    // `command_for(/project/src/a.cpp)` about a database entry spelled `src/a.cpp` and gets nothing.
    let file_as_written = source;
    let file = PathBuf::from(&file_as_written);
    let file = match (&directory, file.is_absolute()) {
        (Some(directory), false) => directory.join(&file),
        _ => file,
    };

    Some(CompileCommand {
        file,
        file_as_written,
        directory,
        arguments,
    })
}

/// The raw text of every object inside every array whose key is `name` — a small, string-aware brace walk.
///
/// `top_level_objects` reads one array at the top of the document (a compile database's shape). A build database nests
/// its entries two levels down, and rather than teach one scanner about nesting this walks the document for each array
/// the key names and brackets the objects inside it.
fn objects_in_every_array_named(json: &str, name: &str) -> Vec<String> {
    let key = format!("\"{name}\"");
    let mut objects = Vec::new();
    let mut searched = 0usize;

    while let Some(found) = json[searched..].find(&key) {
        let after_key = searched + found + key.len();
        searched = after_key;
        let Some(array) = json[after_key..].find('[') else {
            break;
        };
        let start = after_key + array;
        // **Two counters, because a `{` inside an entry is not another entry.** `provides` and `requires` are objects
        // and arrays of their own, and one counter would take them for the next translation unit — measured: a
        // database with two units in one set read as one command.
        let mut brackets = 0usize;
        let mut braces = 0usize;
        let mut object_start: Option<usize> = None;
        let mut in_string = false;
        let mut escaped = false;

        for (offset, character) in json[start..].char_indices() {
            let index = start + offset;
            if in_string {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_string = false;
                }
                continue;
            }
            match character {
                '"' => in_string = true,
                '[' => brackets += 1,
                ']' => {
                    brackets = brackets.saturating_sub(1);
                    if brackets == 0 {
                        searched = index + 1;
                        break;
                    }
                }
                '{' => {
                    braces += 1;
                    if brackets == 1 && braces == 1 {
                        object_start = Some(index);
                    }
                }
                '}' => {
                    braces = braces.saturating_sub(1);
                    if brackets == 1
                        && braces == 0
                        && let Some(from) = object_start.take()
                    {
                        objects.push(json[from..=index].to_string());
                    }
                }
                _ => {}
            }
        }
    }

    objects
}

/// A parsed compile database, and what was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompileCommands {
    pub commands: Vec<CompileCommand>,
    /// How many entries were present but not understood.
    ///
    /// Reported rather than ignored: a database where most entries failed to parse is one whose include
    /// paths are mostly missing, and an analysis built on it would be quietly wrong rather than loudly
    /// incomplete.
    pub malformed: usize,
}

impl CompileCommands {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// **The one standard every command that states one agrees on**, or None.
    ///
    /// A header has no translation unit of its own, and the standard is what decides which half of every guarded
    /// header exists. Measured on a `<ranges>` fixture whose only database entry is
    /// \... /std:c++latest ...\ for \main.cpp\: \<ranges>\'s own config had no command, so it had no standard,
    /// so \_HAS_CXX20\ was false and the cooked rendering of the header was the **12-byte** \#pragma once\ stub —
    /// the pipeline's type could never be inferred. A project where the entries disagree is one where the header
    /// really is built two ways, and then this answers \None\ rather than picking one.
    pub fn a_unanimous_standard(&self) -> Option<&str> {
        let mut found: Option<&str> = None;
        for command in &self.commands {
            let standard = command.arguments.iter().find_map(|argument| {
                argument
                    .strip_prefix("/std:")
                    .or_else(|| argument.strip_prefix("-std="))
                    .or_else(|| argument.strip_prefix("-std:"))
            })?;
            match found {
                None => found = Some(standard),
                Some(held) if held == standard => {}
                Some(_) => return None,
            }
        }
        found
    }

    /// The command for a file, if the database has one.
    ///
    /// Compared by the *tail* of the path, and falling back to the tail is the point: a database writes
    /// absolute paths from the machine that produced it, and a project opened somewhere else has the same
    /// files with a different prefix — a different checkout directory, a container path, a drive letter.
    /// Requiring an exact match would make a checked-in `compile_commands.json` useless, which is the case
    /// it is most often generated for.
    ///
    /// The two are matched by their **longest common tail**: the request's tails are collected into a set
    /// and the stored path's tails are looked up in it. Comparing only one side's tails would match in one
    /// direction — a request with a prefix the database lacks, but not a database entry with a prefix the
    /// request lacks — and the two directions are the same question asked twice.
    pub fn command_for(&self, file: &Path) -> Option<&CompileCommand> {
        // **Exact spelling first**, and case-folding only when nothing matched — a database from another machine may
        // spell a path with a different case, but one that names both `Foo.cpp` and `foo.cpp` means both
        // (`docs/worklog.md` P2-9).
        self.command_for_with(file, false)
            .or_else(|| self.command_for_with(file, true))
    }

    /// [`CompileCommands::command_for`] with the case policy stated — see its note.
    fn command_for_with(&self, file: &Path, case_insensitive: bool) -> Option<&CompileCommand> {
        let requested = normalize_for_comparison(file, case_insensitive);
        let request_tails: std::collections::HashSet<&str> =
            path_suffixes(&requested).into_iter().collect();

        // The most specific match wins, so a database with both `/a/x.h` and `/b/a/x.h` answers a request
        // for the latter with the entry that names more of it.
        let mut best: Option<(usize, &CompileCommand)> = None;

        for command in &self.commands {
            let stored = normalize_for_comparison(&command.file, case_insensitive);

            for tail in path_suffixes(&stored) {
                if request_tails.contains(tail) {
                    let specificity = tail.len();
                    if best.is_none_or(|(best_len, _)| specificity > best_len) {
                        best = Some((specificity, command));
                    }
                    break;
                }
            }
        }

        best.map(|(_, command)| command)
    }

    /// **A command for a file the database has no entry for** — a header, in practice.
    ///
    /// A build lists its *sources*; a header is compiled as part of one of them and appears nowhere. So
    /// [`CompileCommands::command_for`] answers `None` for it, and the project-wide configuration that answers
    /// instead knows nothing about the `-I`/`-isystem` flags the build passed — measured on neovim: `src/nvim/os/fs.h`
    /// includes `<uv.h>`, the header has no entry, and the `-isystem .deps/usr/include` that resolves it is in the
    /// response file of `src/nvim/os/fs.c`. **Eleven** headers of one closure reported `uv.h` unresolved, and one
    /// unresolved include is what takes the macro environment's completeness claim back (`Marked::mark_incomplete`)
    /// for every file whose closure reaches it.
    ///
    /// The entry borrowed is a source **in the same directory**, the smallest path so the answer is deterministic.
    /// Same directory and not "any entry": a database may describe several targets with different include roots, and
    /// a header is the file the source beside it is most likely to compile. `None` when the directory has no
    /// source — a header-only tree keeps the project-wide answer.
    pub fn a_command_for_a_header(&self, file: &Path) -> Option<&CompileCommand> {
        self.a_command_for_a_header_with(file, false)
            .or_else(|| self.a_command_for_a_header_with(file, true))
    }

    /// [`CompileCommands::a_command_for_a_header`] with the case policy stated — see
    /// [`CompileCommands::command_for`].
    fn a_command_for_a_header_with(
        &self,
        file: &Path,
        case_insensitive: bool,
    ) -> Option<&CompileCommand> {
        let parent = file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())?;
        let wanted = normalize_for_comparison(parent, case_insensitive);
        let mut best: Option<(String, &CompileCommand)> = None;
        for command in &self.commands {
            let Some(own_parent) = command.file.parent() else {
                continue;
            };
            if normalize_for_comparison(own_parent, case_insensitive) != wanted {
                continue;
            }
            let key = normalize_for_comparison(&command.file, case_insensitive);
            if best.as_ref().is_none_or(|(held, _)| key < *held) {
                best = Some((key, command));
            }
        }
        best.map(|(_, command)| command)
    }
}

/// Every tail of a path that starts at a component boundary, longest first.
///
/// `a/b/c.cpp` yields `a/b/c.cpp`, `b/c.cpp`, `c.cpp`. The boundary is what keeps the match meaningful:
/// a bare `ends_with` would let a request for `in.cpp` find `main.cpp`, which is not a path relationship
/// but a string one.
///
/// A root is part of the boundary rather than a component, so `/a/b.cpp` also yields `a/b.cpp` — a request
/// spelled with an absolute prefix still matches a database entry spelled without one, and the reverse.
fn path_suffixes(path: &str) -> Vec<&str> {
    let mut suffixes = vec![path];

    for (index, character) in path.char_indices() {
        if character == '/' {
            let rest = &path[index + 1..];
            // A root on its own, or a drive root (`C:/`), leaves nothing to match at this boundary.
            if !rest.is_empty() {
                suffixes.push(rest);
            }
        }
    }

    // A drive prefix is a boundary too: `c:/a/b.h` yields `a/b.h`.
    if let Some(index) = path.find(":/")
        && index + 2 < path.len()
    {
        suffixes.push(&path[index + 2..]);
    }

    suffixes
}

/// A path reduced to the form two spellings of one file agree on.
///
/// Separators are unified **before** lexical normalization, and that order matters: on Unix a backslash is
/// an ordinary character in a file name, so `normalize_path` leaves `C:\proj\a.cpp` as one component named
/// `C:\proj\a.cpp`. A database written on Windows and read on Linux — which is what a checked-in
/// `compile_commands.json` usually is — has to be read as a path, not as a file name containing
/// backslashes.
///
/// Case is folded for the same reason: the same file spelled two ways across platforms.
/// **A path's spelling for comparison**, with one separator and, when asked, one case.
///
/// `case_insensitive` is a parameter because the filesystem decides it and this function does not: folding case on a
/// case-sensitive volume makes `Foo.cpp` and `foo.cpp` the same translation unit, so a database that lists both answers
/// whichever it reaches first — a plausible wrong compilation rather than a missing one
/// (`docs/worklog.md` P2-9). [`CompileCommands::command_for`] asks exactly first and folds only as a
/// fallback, which is the same order the review describes: the machine's own rule first, case-folding for a database
/// written on another machine.
fn normalize_for_comparison(path: &Path, case_insensitive: bool) -> String {
    let unified: String = path
        .to_string_lossy()
        .chars()
        .map(|character| if character == '\\' { '/' } else { character })
        .collect();

    crate::paths::normalize_path(Path::new(&unified), case_insensitive)
}

/// The `{...}` objects directly inside the outermost array.
///
/// Deliberately shallow: it tracks brace and string nesting so that a `{` inside a string does not count,
/// and it does not attempt to be a general JSON reader.
fn top_level_objects(json: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for (index, character) in json.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(start) = start.take()
                {
                    objects.push(json[start..=index].to_string());
                }
            }
            _ => {}
        }
    }

    objects
}

/// Read the three fields of one entry.
/// **`file` resolved against `directory`, with `.` dropped and `..` applied.**
///
/// The other database format already joins its relative `source` with its `work-directory`
/// (`parse_translation_unit`), and a `compile_commands.json` may write the pair the same way. Lexical resolution is
/// enough here and is the right answer: the file has to exist for the build, and `..` in a build directory is how a
/// CMake database spells "the source tree one level up" (`directory = /p/build`, `file = ../src/a.cpp`).
///
/// `normalize_path` is not used because it normalises *separators*, not dot segments — `/p/build/../src/a.cpp` would
/// still be spelled with the `..`, and the comparison header borrowing does is on the spelled path.
fn resolve_relative(directory: &Path, file: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in directory.join(file).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    parts.iter().collect()
}

fn parse_command_object(object: &str) -> Option<CompileCommand> {
    let directory = string_field(object, "directory").map(PathBuf::from);
    let file = string_field(object, "file")?;
    // **The spelling, before it is resolved** — what `__FILE__` answers. See [`CompileCommand::file_as_written`].
    let file_as_written = file.clone();

    // Either shape: `arguments` is a list, `command` is a shell string. Both are in the wild, and a
    // project may use either, so neither can be treated as the real one.
    let arguments = match array_field(object, "arguments") {
        Some(arguments) => arguments,
        None => split_command_line(&string_field(object, "command")?),
    };

    // **A relative `file` is relative to `directory`**, exactly as in the other database format
    // (`build_database.json`'s `parse_translation_unit` already joins its `source` with its `work-directory`). It is not
    // only a display path: header borrowing compares the command file's directory with the header's, so a relative
    // command path never matched and the header was analysed without the flags of the source beside it
    // (`docs/worklog.md` P2-8).
    let file = match &directory {
        Some(directory) if Path::new(&file).is_relative() => {
            resolve_relative(directory, Path::new(&file))
        }
        _ => PathBuf::from(file),
    };

    Some(CompileCommand {
        file,
        file_as_written,
        directory,
        arguments,
    })
}

/// The value of a top-level string field.
fn string_field(object: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let after_key = object.find(&key)? + key.len();

    let rest = object[after_key..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;

    let mut value = String::new();
    let mut escaped = false;
    for character in rest.chars() {
        if escaped {
            // The sequences that appear in paths: a Windows separator and a quote.
            value.push(match character {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }

        match character {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }

    None
}

/// The values of a top-level string-array field.
fn array_field(object: &str, name: &str) -> Option<Vec<String>> {
    let key = format!("\"{name}\"");
    let after_key = object.find(&key)? + key.len();

    let rest = object[after_key..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('[')?;

    let mut values = Vec::new();
    let mut current: Option<String> = None;
    let mut escaped = false;

    for character in rest.chars() {
        if escaped {
            if let Some(value) = current.as_mut() {
                value.push(character);
            }
            escaped = false;
            continue;
        }

        match character {
            '\\' => escaped = true,
            '"' => match current.take() {
                Some(value) => values.push(value),
                None => current = Some(String::new()),
            },
            ']' if current.is_none() => return Some(values),
            other => {
                if let Some(value) = current.as_mut() {
                    value.push(other);
                }
            }
        }
    }

    None
}

/// Split a shell command line into arguments.
///
/// Handles the quoting a build system uses: single and double quotes, and backslash escapes. Not a shell
/// — no expansion, no pipes — because a compile command is a program with arguments, and anything more
/// would be reading a language this is not trying to implement.
///
/// # A backslash is a separator before it is an escape
///
/// The rule is **"a backslash escapes only whitespace and a quote"**, and it is one rule rather
/// than two because the same function reads the database on every platform: `\` is the path
/// separator on Windows and the escape character in a POSIX shell, and a reader that treats it as
/// POSIX does everywhere deletes the character after every separator. Every `#include <print>` in
/// it then resolved to the wrong library or to nothing, which is what "the editor does not
/// understand this project" looks like from the outside.
///
/// So a backslash before a letter, a digit or another backslash is **kept**: `C:\Users\zc` is a path, `-I a\ b` is
/// an escaped space, `"a \"quoted\" b"` is a quoted run with two quotation marks in it. `\\server\share` keeps both
/// of its leading separators, which the POSIX rule would have collapsed — the one place this is deliberately wider
/// than a shell, because a UNC path is a real spelling and `\\` after an escape is not.
pub fn split_command_line(command: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut quote: Option<char> = None;
    let mut characters = command.chars().peekable();

    while let Some(character) = characters.next() {
        match character {
            // Before a quote or a space a backslash is an escape; anywhere else it is a path separator.
            '\\' if quote != Some('\'') => {
                let escapes = characters
                    .peek()
                    .is_some_and(|next| next.is_whitespace() || *next == '"' || *next == '\'');
                if escapes {
                    current.push(characters.next().expect("peeked"));
                } else {
                    current.push('\\');
                }
                has_current = true;
            }
            '"' | '\'' if quote.is_none() => {
                quote = Some(character);
                // A quote opens an argument even if it is empty: `"" ` is one empty argument.
                has_current = true;
            }
            character if Some(character) == quote => quote = None,
            character if character.is_whitespace() && quote.is_none() => {
                if has_current {
                    arguments.push(std::mem::take(&mut current));
                    has_current = false;
                }
            }
            other => {
                current.push(other);
                has_current = true;
            }
        }
    }

    if has_current {
        arguments.push(current);
    }

    arguments
}

/// The macros a **configuration decides by itself** — before a single file is read, and whether or not a compiler
/// was found.
///
/// A discovered toolchain's `-dM` output is the fuller answer and stays the base (see `Toolchain::macros`); this is
/// the other half. A compile database that says `-std=c++20` and nothing else has already **decided**
/// `__cplusplus`, and every `#if __cplusplus >= 201703L` in every header is a question about it — a question the
/// condition layer can only answer `Unknown` (and therefore keeps every branch's `#define` out of the evidence)
/// when nobody puts the number there. The same goes for the platform a `--target` triple names.
///
/// # What is deliberately *not* here
///
/// Nothing whose value the configuration does not fix. `__GNUC__`'s **version** is not in `-std=c++20`, and a
/// guessed `__GNUC__ 1` answers `#if __GNUC__ >= 5` with a confident **false** — the one kind of answer this
/// layer must never give, because `Unknown` costs a reading while a wrong `false` loses a branch. The compiler's
/// family and version are the toolchain's to report.
///
/// # Order
///
/// Callers apply these **over** the toolchain's predefined macros and **under** the configuration's own `-D`s: a
/// project that says `-std=c++11` means it, even when the compiler's default invocation would have said
/// `202002L` — which is exactly the point `Toolchain::search_paths` makes about passing the standard on.
pub fn predefined_macros_of(config: &CompilerConfig) -> Vec<CommandLineMacro> {
    let mut macros = Vec::new();

    if let Some(standard) = config.standard.as_deref() {
        if let Some(value) = cplusplus_value(standard) {
            macros.push(CommandLineMacro::with_value("__cplusplus", value));
            // **`-freflection` decides one more macro**, and libstdc++`s `<meta>` is gated on it. GCC 16.2 defines
            // `__cpp_impl_reflection 202603L` with the flag and nothing without it — measured with `-dM -E`.
            if config.reflection {
                macros.push(CommandLineMacro::with_value(
                    "__cpp_impl_reflection",
                    "202603L",
                ));
            }

            // MSVC's own headers ask `_MSVC_LANG` for the same number, and its value is the language version
            // rather than the compiler's — so it is decided by the standard here too.
            if config.dialect() == Dialect::Msvc {
                macros.push(CommandLineMacro::with_value("_MSVC_LANG", value));
            }
        } else if let Some(value) = c_version(standard) {
            macros.push(CommandLineMacro::with_value("__STDC__", "1"));
            macros.push(CommandLineMacro::with_value("__STDC_VERSION__", value));
        }
    }

    if let Some(target) = config.target.as_deref() {
        macros.extend(target_macros(target));
    }

    macros
}

/// The standard with the dialect prefix (`gnu`) taken off: `gnu++20` → `++20`, `c++17` → `c++17`, `gnu11` → `11`.
fn without_the_gnu_prefix(standard: &str) -> &str {
    standard.strip_prefix("gnu").unwrap_or(standard)
}

/// `c++17` → `201703L`, `gnu++20` → `202002L`, and the older `c++2a`/`c++2b` spellings.
///
/// `None` for anything this table does not know — an unset standard, a typo, or a draft whose number is not settled:
/// a wrong `__cplusplus` is worse than an absent one, because absent is `Unknown`.
///
/// **`c++23` and `c++26` are settled now, and it matters.** GCC 16.2 says `202302L` and `202400L` (measured with
/// `-dM -E`), and libstdc++`s `<meta>` is gated on `__cplusplus > 202302L`: while this table answered `202311L` for
/// `c++23` and nothing for `c++26`, the reflection half of every header was a condition the analysis could not
/// decide — so it kept the branch out, and `std::meta` had no declaration to hover.
fn cplusplus_value(standard: &str) -> Option<&'static str> {
    // `-std=c++17` and `-std=gnu++17` differ only in whose extensions are on, and only the first writes the `c`.
    let rest = without_the_gnu_prefix(standard);
    let year = rest
        .strip_prefix("c++")
        .or_else(|| rest.strip_prefix("++"))?;

    Some(match year {
        "98" | "03" => "199711L",
        "11" => "201103L",
        "14" => "201402L",
        "17" => "201703L",
        "20" | "2a" => "202002L",
        "23" | "2b" => "202302L",
        // **MSVC's "newest" writes no year at all.** `/std:c++latest` is what a CMake MSVC build writes, and the
        // table answered `None` for it — so the answer to every `#if _HAS_CXX20` was `Unknown`, and a header's whole
        // C++20 half went unread. `202302L` is the newest *settled* value this table knows, which is the one
        // `c++23` gets; MSVC itself reports `202004L` for `latest` on 19.29 and higher with `/Zc:__cplusplus`, and a
        // header gated on `>= 202002L` is the question this has to answer. Measured on `<ranges>`: the rendering
        // went from the 206-byte fallback to the header's real body once this arm existed.
        "latest" | "preview" => "202302L",
        // **C++26 is settled for this compiler**: `g++ 16.2 -std=c++26 -dM -E` says `202400L`, and libstdc++`s
        // `<meta>` is gated on `__cplusplus > 202302L` — absent was a branch the analysis could not decide.
        "26" | "2c" => "202400L",
        _ => return None,
    })
}

/// `c11` → `201112L`, `gnu99` → `199901L`. `None` for a C++ standard or an unknown spelling.
fn c_version(standard: &str) -> Option<&'static str> {
    // `-std=c11` and `-std=gnu11` are the same standard, and only the first has a `c` to take off.
    let rest = without_the_gnu_prefix(standard);
    let year = rest.strip_prefix('c').unwrap_or(rest);

    Some(match year {
        "89" | "90" => "199409L",
        "99" => "199901L",
        "11" => "201112L",
        "17" | "18" => "201710L",
        // C23 is `202311L` and C2y is `202500L` — measured with `gcc -std=c23 -dM -E` and `-std=c2y`.
        "23" | "2x" => "202311L",
        "2y" => "202500L",
        _ => return None,
    })
}

/// The platform names a target triple — or `-m32`/`-m64` — decides.
///
/// Only the names the target **fixes**: the operating system and the pointer width. Sizes, versions and the
/// compiler's family are not in a triple's spelling, and guessing them is what [`predefined_macros_of`] refuses.
fn target_macros(target: &str) -> Vec<CommandLineMacro> {
    let lowered = target.to_ascii_lowercase();
    let mut macros = Vec::new();

    // 64- and 32-bit are marked by the **architecture** name, which is why the answer is a pair: `_WIN64` is about
    // the target being a 64-bit one, and `x86_64-w64-mingw32`'s `w64` is the *vendor's* spelling — a `i686-w64-…`
    // target is 32-bit with the same vendor string, which is how a `contains("64")` test gets it wrong.
    let architecture =
        if lowered.contains("x86_64") || lowered.contains("amd64") || lowered == "-m64" {
            Some(("__x86_64__", true))
        } else if lowered.contains("i686")
            || lowered.contains("i386")
            || lowered.contains("x86")
            || lowered == "-m32"
        {
            Some(("__i386__", false))
        } else if lowered.contains("aarch64") || lowered.contains("arm64") {
            Some(("__aarch64__", true))
        } else if lowered.contains("arm") {
            Some(("__arm__", false))
        } else {
            None
        };

    let is_windows =
        lowered.contains("windows") || lowered.contains("mingw") || lowered.contains("msvc");

    if let Some((name, _)) = architecture {
        macros.push(CommandLineMacro::with_value(name, "1"));
    }

    if is_windows {
        macros.push(CommandLineMacro::with_value("_WIN32", "1"));
        if architecture.is_some_and(|(_, wide)| wide) {
            macros.push(CommandLineMacro::with_value("_WIN64", "1"));
        }
    }

    if lowered.contains("apple") || lowered.contains("darwin") {
        macros.push(CommandLineMacro::with_value("__APPLE__", "1"));
    }

    if lowered.contains("linux") {
        macros.push(CommandLineMacro::with_value("__linux__", "1"));
    }

    macros
}

#[cfg(test)]
mod tests {
    /// **A database that names both `Foo.cpp` and `foo.cpp` means both.** Matching used to fold case before comparing,
    /// so on a case-sensitive filesystem — where the two are different translation units — a request for either could
    /// select the other's command: a plausible wrong compilation rather than a missing one
    /// (`docs/worklog.md` P2-9).
    #[test]
    fn an_exact_case_match_wins_over_a_folded_one() {
        let database = crate::config::parse_compile_commands(
            r#"[
                {"directory": "/p", "file": "/p/src/Foo.cpp", "arguments": ["clang++", "-DUPPER", "-c", "src/Foo.cpp"]},
                {"directory": "/p", "file": "/p/src/foo.cpp", "arguments": ["clang++", "-Dlower", "-c", "src/foo.cpp"]}
            ]"#,
        );

        let upper = database
            .command_for(Path::new("/p/src/Foo.cpp"))
            .expect("the upper-case entry");
        assert!(
            upper.arguments.iter().any(|argument| argument == "-DUPPER"),
            "the exact spelling is the answer: {:?}",
            upper.arguments
        );

        let lower = database
            .command_for(Path::new("/p/src/foo.cpp"))
            .expect("the lower-case entry");
        assert!(
            lower.arguments.iter().any(|argument| argument == "-Dlower"),
            "and so is the other one: {:?}",
            lower.arguments
        );

        // **The fallback still works**: a request whose case the database does not use matches by folding, which is what
        // makes a database written on another machine usable. *Which* of the two it answers is not decided here - a
        // folded comparison cannot tell them apart, and that ambiguity is exactly why the exact pass runs first.
        let shouted = database
            .command_for(Path::new("/p/src/FOO.CPP"))
            .expect("a case-folded match");
        assert!(
            shouted
                .arguments
                .iter()
                .any(|argument| argument == "-DUPPER")
                || shouted
                    .arguments
                    .iter()
                    .any(|argument| argument == "-Dlower"),
            "either entry, by folding: {:?}",
            shouted.arguments
        );
    }

    /// **A relative `file` in a `compile_commands.json` is relative to `directory`.** The other database format already
    /// joined its pair, and this one did not — which is not only a display path: header borrowing compares the command
    /// file's directory with the header's, so a relative command path never matched and a header was analysed without the
    /// flags of the source beside it (`docs/worklog.md` P2-8).
    #[test]
    fn a_relative_command_file_is_resolved_against_its_directory() {
        let database = crate::config::parse_compile_commands(
            r#"[{"directory": "/p/build", "file": "../src/a.cpp", "arguments": ["clang++", "-DFROM_BUILD", "-c", "../src/a.cpp"]}]"#,
        );

        let command = database
            .command_for(Path::new("/p/src/a.cpp"))
            .expect("the command is found by the source path it builds");

        assert_eq!(
            command.file,
            Path::new("/p/src/a.cpp"),
            "the `..` is applied, not left spelled in the path"
        );
        assert_eq!(
            command.file_as_written, "../src/a.cpp",
            "and the spelling a compiler would report for `__FILE__` is kept beside it"
        );
        assert!(
            command
                .arguments
                .iter()
                .any(|argument| argument == "-DFROM_BUILD"),
            "and the flags are the ones that command carried: {:?}",
            command.arguments
        );
    }

    /// **The name `__FILE__` answers is the database's spelling, not the resolved path.** A compiler reports the
    /// name it was invoked with; the index needs one identity per file, so the command keeps both.
    #[test]
    fn the_presumed_name_is_the_spelling_the_database_wrote() {
        let database = crate::config::parse_compile_commands(
            r#"[{"directory": "/p/build", "file": "../src/a.cpp", "arguments": ["clang++", "-c"]}]"#,
        );
        let config = CompilerConfig {
            database: Some(std::sync::Arc::new(database)),
            ..CompilerConfig::default()
        };

        assert_eq!(
            config.presumed_name_for(Path::new("/p/src/a.cpp")),
            Some(Path::new("../src/a.cpp")),
            "the command's spelling is kept for `__FILE__`"
        );
        assert_eq!(
            config.presumed_name_for(Path::new("/p/src/other.cpp")),
            None,
            "a file no command names has no presumed name; the caller uses its path"
        );
    }

    /// **MSVC's `/I`, `/D` and `/U`, joined and separated.** A `cl.exe` database loses its include directories and its
    /// feature macros without these, and the loss is silent: conditionals take the other branch and generated headers
    /// are not found (`docs/worklog.md` P1-10).
    #[test]
    fn msvc_include_define_and_undef_arguments_are_read() {
        let arguments: Vec<String> = [
            "cl.exe",
            "/c",
            "/Igenerated",
            "/I",
            "extra/code",
            "/DWIN32",
            "/D",
            "FEATURE=1",
            "/UOLD",
            "/U",
            "OLDER",
            "/I-",
            "/DEBUG",
            "src\\a.cpp",
        ]
        .iter()
        .map(|it| it.to_string())
        .collect();

        let config = config_from_arguments(&arguments, None);

        let include_directories: Vec<&str> = config
            .include_paths
            .iter()
            .map(|path| path.directory.to_string_lossy().into_owned().leak() as &str)
            .collect();
        assert_eq!(
            include_directories,
            ["generated", "extra/code"],
            "joined and separated, and `/I-` is not a directory"
        );
        let defines: Vec<String> = config
            .defines
            .iter()
            .map(|definition| definition.name.to_string())
            .collect();
        assert_eq!(defines, ["WIN32", "FEATURE"], "`/DEBUG` is not `/D EBUG`");
        assert_eq!(
            config
                .undefines
                .iter()
                .map(|name| name.as_ref())
                .collect::<Vec<_>>(),
            ["OLD", "OLDER"]
        );
    }

    use super::*;

    /// The standard is a **decision**, and the one every header asks about.
    #[test]
    fn the_standard_decides_cplusplus() {
        for (standard, expected) in [
            ("c++98", "199711L"),
            ("gnu++03", "199711L"),
            ("c++11", "201103L"),
            ("gnu++14", "201402L"),
            ("c++17", "201703L"),
            ("gnu++20", "202002L"),
            ("c++2a", "202002L"),
            ("c++23", "202302L"),
            ("c++26", "202400L"),
        ] {
            let config = CompilerConfig::default().with_standard(standard);
            let macros = predefined_macros_of(&config);
            let cplusplus = macros.iter().find(|entry| &*entry.name == "__cplusplus");

            assert_eq!(
                cplusplus.and_then(|entry| entry.value.as_deref()),
                Some(expected),
                "{standard} means {expected}"
            );
        }

        // A standard this table does not know, and no standard at all: **nothing** is said, which is `Unknown`
        // rather than a wrong number. `c++26` used to be in this list — it is settled now (`202400L`, measured on
        // GCC 16.2), and the draft after it is not.
        for standard in ["c++29", "c90", "nonsense"] {
            let config = CompilerConfig::default().with_standard(standard);
            assert!(
                !predefined_macros_of(&config)
                    .iter()
                    .any(|entry| &*entry.name == "__cplusplus"),
                "{standard} says nothing about `__cplusplus`"
            );
        }
    }

    /// **MSVC writes the standard with a slash and a colon**, and a compile database on Windows holds nothing else.
    ///
    /// Measured on a `<ranges>` fixture whose only entry is `... /std:c++latest ...`: the flag was not read, the
    /// standard stayed unset, `_HAS_CXX20` answered false, and the cooked rendering of `<ranges>` was the 12-byte
    /// `#pragma once` stub — `std::views` did not exist and every pipeline answered `UnknownType`.
    #[test]
    fn the_msvc_spelling_of_the_standard_is_read() {
        for spelling in ["/std:c++latest", "/std:c++20", "-std:c++17"] {
            let arguments = vec![spelling.to_string()];
            let config = config_from_arguments(&arguments, None);
            assert!(config.standard.is_some(), "{spelling} states a standard");
        }

        let arguments = vec!["/std:c++latest".to_string()];
        let config = config_from_arguments(&arguments, None);
        assert_eq!(config.standard.as_deref(), Some("c++latest"));
        let macros = predefined_macros_of(&config);
        assert_eq!(
            macros
                .iter()
                .find(|entry| &*entry.name == "__cplusplus")
                .and_then(|entry| entry.value.as_deref()),
            Some("202302L"),
            "`c++latest` is a number, not silence: every `#if _HAS_CXX20` depends on it"
        );
    }

    /// **A header with no command of its own inherits a unanimous project standard** — and only a unanimous one.
    #[test]
    fn a_header_inherits_a_unanimous_standard_and_not_a_split_one() {
        let command = |file: &str, standard: &str| CompileCommand {
            directory: None,
            file: PathBuf::from(file),
            file_as_written: file.to_string(),
            arguments: vec![format!("/std:{standard}"), "/c".to_string()],
        };

        let unanimous = CompileCommands {
            commands: vec![
                command("/p/a.cpp", "c++latest"),
                command("/p/b.cpp", "c++latest"),
            ],
            malformed: 0,
        };
        assert_eq!(unanimous.a_unanimous_standard(), Some("c++latest"));

        let split = CompileCommands {
            commands: vec![command("/p/a.cpp", "c++17"), command("/p/b.cpp", "c++20")],
            malformed: 0,
        };
        assert_eq!(
            split.a_unanimous_standard(),
            None,
            "a header built two ways has no one standard to inherit"
        );
    }

    /// MSVC's headers ask the same question under a different name.
    #[test]
    fn the_msvc_dialect_gets_the_language_version_too() {
        let config = CompilerConfig::default()
            .with_standard("c++17")
            .with_dialect(Dialect::Msvc);
        let macros = predefined_macros_of(&config);

        assert_eq!(
            macros
                .iter()
                .find(|entry| &*entry.name == "_MSVC_LANG")
                .and_then(|entry| entry.value.as_deref()),
            Some("201703L")
        );
        // And GCC's dialect does not: a name that compiler never defines is a name its headers never ask about.
        let config = CompilerConfig::default().with_standard("c++17");
        assert!(
            !predefined_macros_of(&config)
                .iter()
                .any(|entry| &*entry.name == "_MSVC_LANG")
        );
    }

    /// C is a different language with a different macro — and its own spellings.
    #[test]
    fn a_c_standard_decides_the_c_version() {
        let config = CompilerConfig::default().with_standard("gnu11");
        let macros = predefined_macros_of(&config);

        let value = |name: &str| {
            macros
                .iter()
                .find(|entry| &*entry.name == name)
                .and_then(|entry| entry.value.as_deref())
        };

        assert_eq!(value("__STDC_VERSION__"), Some("201112L"));
        assert_eq!(value("__STDC__"), Some("1"));
        assert_eq!(value("__cplusplus"), None, "C has no `__cplusplus`");
    }

    /// The target decides the platform, and **only** what it says.
    #[test]
    fn the_target_decides_the_platform_names() {
        let names = |target: &str| {
            predefined_macros_of(&CompilerConfig::default().with_target(target))
                .into_iter()
                .map(|entry| entry.name.to_string())
                .collect::<Vec<_>>()
        };

        assert!(names("x86_64-w64-mingw32").contains(&"_WIN32".to_string()));
        assert!(names("x86_64-w64-mingw32").contains(&"_WIN64".to_string()));
        assert!(names("x86_64-w64-mingw32").contains(&"__x86_64__".to_string()));
        assert!(names("x86_64-unknown-linux-gnu").contains(&"__linux__".to_string()));
        assert!(!names("x86_64-unknown-linux-gnu").contains(&"_WIN32".to_string()));
        assert!(names("i686-w64-mingw32").contains(&"__i386__".to_string()));
        assert!(!names("i686-w64-mingw32").contains(&"_WIN64".to_string()));
        assert!(names("-m32").contains(&"__i386__".to_string()));
        assert!(names("-m64").contains(&"__x86_64__".to_string()));

        // A target nobody described says nothing: the platform macros are a claim about the *target*, and a
        // spelling this table does not know is not one.
        assert!(names("wasm32-unknown-unknown").is_empty());
    }
}
/// **One definition as a configuration file writes it** — `"NAME"` or `"NAME=value"`, without the `-D`.
///
/// The same reading `-D` gets on a command line, because the two say the same thing in two places: `#if NAME`
/// depends on which of the two spellings was used.
fn define_from_a_config_file(written: &str) -> CommandLineMacro {
    match written.split_once('=') {
        Some((name, value)) => CommandLineMacro::with_value(name.trim(), value),
        None => CommandLineMacro::defined(written.trim()),
    }
}
#[cfg(test)]
mod database_search_path_tests {
    use super::*;

    /// **A header borrows the include directories of the source beside it.**
    ///
    /// A build lists its *sources*: a header has no entry of its own, so `command_for` answers `None` and the
    /// project-wide configuration answers — without the `-isystem` directory the build passed. Measured on neovim:
    /// **eleven headers** of one closure reported `<uv.h>` unresolved although `src/nvim/os/fs.c` beside them
    /// resolves it from its response file, and one unresolved include takes the macro environment's completeness
    /// claim back (`Marked::mark_incomplete`) for every file whose closure reaches it.
    #[test]
    fn a_header_borrows_the_include_directories_of_a_source_beside_it() {
        let root = PathBuf::from("/p");
        let source = root.join("os/fs.c");
        let header = root.join("os/fs.h");
        let database = CompileCommands {
            commands: vec![CompileCommand {
                file: source.clone(),
                file_as_written: source.to_string_lossy().into_owned(),
                directory: Some(root.clone()),
                arguments: vec![
                    "cc".to_string(),
                    "-isystem".to_string(),
                    "/p/.deps/include".to_string(),
                    "-I/p/src".to_string(),
                    "-c".to_string(),
                    source.to_string_lossy().into_owned(),
                ],
            }],
            malformed: 0,
        };
        let config = CompilerConfig {
            root: Some(root.clone()),
            database: Some(std::sync::Arc::new(database)),
            ..CompilerConfig::default()
        };

        let per_file = config.for_file(&header);
        let directories: Vec<&Path> = per_file
            .include_paths
            .iter()
            .map(|path| path.directory.as_path())
            .collect();
        assert!(
            directories.contains(&Path::new("/p/.deps/include")),
            "the header is compiled by the source beside it: {directories:?}"
        );
        assert!(directories.contains(&Path::new("/p/src")));

        // …and a header with no source beside it keeps the project-wide answer, which is the honest `None`.
        let lonely = config.for_file(&root.join("include/api.h"));
        assert!(
            lonely.include_paths.is_empty(),
            "nothing beside it says anything: {:?}",
            lonely
                .include_paths
                .iter()
                .map(|path| path.directory.as_path())
                .collect::<Vec<_>>()
        );
    }

    /// **A compile database entry replaces the project's directories, never the compiler's.**
    ///
    /// Reported from a project with `compile_commands.json`: "几乎不认识 C++ 标准库了". The database's commands were
    /// `c++ -std=c++23 ... -c src/main.cpp` — **no `-I` at all**, because a compiler's own search path is built in and
    /// never written on the command line. Taking that entry's (empty) directory list as the file's whole search path
    /// therefore deleted `<string>`, `<print>`, `<expected>` and `<windows.h>` from every file of the project: four
    /// `IncludeNotFound` diagnostics and no type for `std::expected<double, std::string>`.
    ///
    /// A project that is *not* behind a database was unaffected — which is why this survived: without one, the
    /// toolchain's paths are the only ones there are.
    #[test]
    fn the_compilers_own_directories_survive_an_empty_database_entry() {
        let root = PathBuf::from("/p");
        let file = root.join("src/main.cpp");
        let database = CompileCommands {
            commands: vec![CompileCommand {
                file: file.clone(),
                file_as_written: file.to_string_lossy().into_owned(),
                directory: Some(root.clone()),
                // The shape CMake's Ninja generator writes: the standard, warnings, and the input file.
                arguments: vec![
                    "c++".to_string(),
                    "-std=c++23".to_string(),
                    "-c".to_string(),
                    file.to_string_lossy().into_owned(),
                ],
            }],
            malformed: 0,
        };

        let config = CompilerConfig {
            root: Some(root.clone()),
            database: Some(std::sync::Arc::new(database)),
            include_paths: vec![
                IncludePath::user("/project/include"),
                IncludePath::system("/gcc/include/c++/15.1.0"),
                IncludePath::system("/gcc/include-fixed"),
            ],
            ..CompilerConfig::default()
        };

        let per_file = config.for_file(&file);
        let directories: Vec<&Path> = per_file
            .include_paths
            .iter()
            .map(|path| path.directory.as_path())
            .collect();

        // The project's own directory is the *entry's* to replace — it is gone, because that entry lists none.
        assert!(
            !directories.contains(&Path::new("/project/include")),
            "the database entry replaces the project's directories: {directories:?}"
        );
        // The compiler's are not, and they come after whatever the entry did say.
        assert_eq!(
            directories,
            vec![
                Path::new("/gcc/include/c++/15.1.0"),
                Path::new("/gcc/include-fixed")
            ],
            "the toolchain's directories are the one part of the search path the entry knows nothing about"
        );
        assert!(
            per_file.include_paths.iter().all(|path| path.is_system),
            "and they keep their kind, so `#include <…>` still finds them"
        );
    }

    /// **A database entry's own `-I` comes first**, and the compiler's directories after it — the order a compiler
    /// searches, and the reason the fix appends rather than prepends.
    #[test]
    fn the_entries_own_directories_come_before_the_compilers() {
        let root = PathBuf::from("/p");
        let file = root.join("src/main.cpp");
        let database = CompileCommands {
            commands: vec![CompileCommand {
                file: file.clone(),
                file_as_written: file.to_string_lossy().into_owned(),
                directory: Some(root.clone()),
                arguments: vec![
                    "c++".to_string(),
                    "-I/project/generated".to_string(),
                    "-c".to_string(),
                    file.to_string_lossy().into_owned(),
                ],
            }],
            malformed: 0,
        };

        let config = CompilerConfig {
            root: Some(root),
            database: Some(std::sync::Arc::new(database)),
            include_paths: vec![IncludePath::system("/gcc/include")],
            ..CompilerConfig::default()
        };

        let directories: Vec<PathBuf> = config
            .for_file(&file)
            .include_paths
            .iter()
            .map(|path| path.directory.clone())
            .collect();
        assert_eq!(
            directories,
            vec![
                PathBuf::from("/project/generated"),
                PathBuf::from("/gcc/include")
            ]
        );
    }
}
