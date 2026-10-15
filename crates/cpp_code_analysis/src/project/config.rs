//! The project's own configuration file, `.cppls.toml`.
//!
//! Everything else in this crate is *derived*: the compile database says how a project is built, the toolchain says
//! where its headers are, and the scan says which files are sources. This file is the one place a **person** says
//! something the analysis cannot work out on its own — "the build tree is not source", "this one is built with
//! `-DFEATURE=1`", "look for the database in `build/`" — and the one place a *guess* can be overridden.
//!
//! ```toml
//! [workspace]
//! exclude = ["**/build/**", "third_party/**"]     # what the analysis should not read
//! source_extensions = ["cuh", "cppm"]             # extensions beyond the engine's own list
//!
//! [compile]
//! database = "build/compile_commands.json"        # where the compile database is, if discovery guesses wrong
//! compiler = "clang++"                            # or a path; absent means "the toolchain discovery decides"
//! args = ["-std=c++20", "-Iinclude"]              # used **instead of** the database's flags when present
//! extra_args = ["-DFEATURE=1"]                    # added on top of whichever flags won
//! remove_args = ["-Werror"]                       # dropped from whichever flags won
//!
//! [index]
//! cache_dir = ".cppls"
//! max_files = 20000
//!
//! [diagnostics]                                   # read by the language server, not by the analysis
//! on_change_ms = 500
//!
//! [hover]
//! enable = true
//! ```
//!
//! # Measured, on a project with no compile database at all
//!
//! `E:\EmmyLuaCodeStyle` — 198 C++ sources in nine CMake targets, plus a `3rd/` tree, **no
//! `compile_commands.json`** because nobody had configured it with `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`. Its
//! includes are written against each target's include directory (`#include "Session/StandardIOSession.h"` from
//! `CodeFormatServer/src/`), which is what `target_include_directories` means and what a compile database would
//! have said.
//!
//! ```text
//!                                    without .cppls.toml   with [compile] args
//!   include paths the resolver had   8                     21
//!   summaries refused by the cache   1046 of 1729          235 of 1729
//!   index time                       16.0 s                20.0 s
//! ```
//!
//! The 13 `-I` flags are the project's own layout, written out once. **The time went up because the analysis is
//! doing more**: an unresolved include refuses the whole summary (`has_unresolved_includes`), so before, the work
//! was *skipped* rather than saved — 1046 files were read, found to be unmemorable, and would be read again next
//! time.
//!
//! The 235 that remain are files reaching headers this machine genuinely does not have (MSVC's own
//! `xmmintrin.h` asks for `xmm_func.h`). That is the honest floor without a database, and it is why a
//! `compile_commands.json` is still the better answer when a project has one.
//!
//! # Where a guess sits, and where it must not
//!
//! **A person's file wins; discovery is a fallback; and no guess is load-bearing.** The order is:
//!
//! ```text
//!   [compile] args          a person said exactly what the compiler is given — and it is used *instead of* the
//!                           database's flags, so this is the override and not a hint
//!   the compile database    the compiler's own answer, per file
//!   toolchain discovery     the system's headers, from the compiler that was found
//!   inferred layout         `include_roots_above` in `crate::include` — a *narrow* rule (an ancestor directory
//!                           holding `include/`) so that the common single-target layout resolves before anyone has
//!                           written a configuration file
//! ```
//!
//! The last of those is the only guess, it is deliberately narrow, and **it does not cover the layout this project
//! has**: its include roots are *siblings* (`CodeFormatCore/include`, `LuaParser/include`) and grandchildren
//! (`3rd/asio-1.24.0/include`) of the file, not ancestors — so the rule finds nothing there and the configuration
//! file is what resolves them. A rule broad enough to find those would be searching a checkout for any directory
//! named `include`, which is the kind of guess that silently picks the wrong file. It is not written, and the
//! table above is what it costs to write thirteen paths down instead.
//!
//! # Why the parse is section-by-section
//!
//! A file a person edits will eventually have a typo in it, and the useful answer to a typo is "the key `exlude` in
//! `[workspace]` is not one I know, and here is what I did with the rest" — not "the configuration could not be
//! read, so everything is default". So the text is read into a `toml::Value` once, and then **each section is
//! decoded on its own**: a section that fails to decode is reported and left at its defaults, and every other
//! section is applied. An unknown *section* is a problem too, listed by name, because a mistyped section header is
//! the typo that would otherwise be the most silent of all.
//!
//! # Why the file is not written back
//!
//! This crate never writes the file. A configuration file belongs to the project, it is committed, it has the
//! user's comments in it — so the analysis reads it and reports what it thinks, and any future "fix my config"
//! feature has to propose a patch rather than rewrite what it did not author.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::file::paths::FileProvider;

/// The name a project's configuration is conventionally found under, in the workspace root.
pub const PROJECT_CONFIG_FILE: &str = ".cppls.toml";

/// Everything `.cppls.toml` can say, after parsing.
///
/// The sections that only a language server reads ([`DiagnosticsSection`], [`HoverSection`]) are here as **data**:
/// this crate parses and validates one file, so that two crates cannot disagree about what it says — the meaning
/// of `diagnostics.on_change_ms` lives in the server, and what lives here is the key and its type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    pub workspace: WorkspaceSection,
    pub compile: CompileSection,
    pub index: IndexSection,
    pub diagnostics: DiagnosticsSection,
    pub hover: HoverSection,
}

/// `[workspace]` — what the project is made of.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSection {
    /// Glob patterns the analysis should not read: a build tree, a vendored copy, generated sources.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extensions **added** to the engine's own source list.
    ///
    /// Added rather than replacing, because the engine's list is what a C++ project is made of and a project that
    /// writes `source_extensions` is saying "and also these" — a CUDA `.cuh` or a module `.cppm`. Replacing the
    /// list is what `exclude` is for, one file at a time.
    #[serde(default)]
    pub source_extensions: Vec<String>,
}

/// `[compile]` — how this project is built, when the discovered answer is not the right one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompileSection {
    /// The compile database to read, relative to the workspace root (or absolute).
    ///
    /// The escape hatch for discovery: a project whose `compile_commands.json` lives somewhere no convention
    /// predicts — a hand-written CMake build directory, a tool that emits it into `.cache/`.
    #[serde(default)]
    pub database: Option<PathBuf>,
    /// The compiler to use, as a name on `PATH` or a path. `"auto"` (or absent) leaves it to the discovery order.
    #[serde(default)]
    pub compiler: Option<String>,
    /// The language standard the whole project is built with, as `-std=` spells it (`"c++20"`).
    ///
    /// The project-wide default a `[[compile.target]]` inherits when it states none, and — like `args` — the
    /// person's word: it is applied ahead of what the compile database's command line says, because a build's
    /// `-std=gnu++20` is what the *build* ran rather than what the project means.
    ///
    /// This exists as its own key rather than as an `args` entry because it is the one flag nearly every project
    /// states and nearly every target restates, and writing `-std=c++20` into a list of flags hides it among the
    /// `-W`/`-O` noise. See [`TargetSection::standard`] for the per-target form.
    #[serde(default)]
    pub standard: Option<String>,
    /// The flags to compile with, **instead of** the compile database's.
    ///
    /// # What this costs, and why it is still the right default
    ///
    /// When this key is present the database's flags are ignored entirely: a project that writes `args` has said
    /// what its compilation is, and merging two sets of flags would make the result depend on their order (two
    /// `-std=` options, a `-I` before and after) in a way nobody can predict by reading the file. The cost is real
    /// — change a `target_compile_definitions` in `CMakeLists.txt` and this file has to be updated by hand — and
    /// the way out is `extra_args`, which adds to whichever flags won. It records the
    /// decision and the alternatives.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Flags added on top of whichever set won, in this order.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Flags dropped from whichever set won, matched by their **whole** spelling (`-Werror`, `-std=c++17`).
    #[serde(default)]
    pub remove_args: Vec<String>,
    /// Extra include directories, by the directory they apply to; `"*"` applies to the whole project.
    ///
    /// Paths are relative to the workspace root. This exists for the project that compiles one subtree with an
    /// extra `-I` — a legacy module, a generated header directory — which is exactly the case a single project-wide
    /// configuration cannot express and a per-file database can.
    #[serde(default)]
    pub include: BTreeMap<String, Vec<String>>,
    /// **Find an `#include` by the tail of its path when no include directory could have.**
    ///
    /// `None` means "as the analysis decides", which is off — a guess must never shadow what a project stated. Set
    /// it `true` for a project whose build system cannot state its include directories at all: a Visual Studio
    /// solution, whose database CMake cannot export, or a checkout nobody has configured.
    ///
    /// See [`crate::include::fuzzy`] for the rule. It runs only after every exact candidate has failed, and it
    /// refuses a target with no directory in it (`#include <vector>`), which is what keeps it from answering a
    /// question the search path was supposed to answer.
    #[serde(default)]
    pub fuzzy_includes: Option<bool>,
    /// **The project's compilation *targets*** — see [`TargetSection`].
    ///
    /// The key that makes this file usable on a real C++ project, because a real C++ project is not one
    /// compilation: it is several, each with its own include directories and its own defines, and a single `args`
    /// list cannot describe them. Writing every target's `-I` into one list is not a simplification but a wrong
    /// answer — a header reachable only from another target's directory then resolves, and an include of
    /// `"Config.h"` picks whichever `Config.h` the search order reaches first.
    ///
    /// Spelled `[[compile.target]]`, one table per target. A file is described by the target whose `sources` match
    /// it most specifically; a file no target matches is described by `[compile]` itself.
    /// Spelled [[compile.target]] — one table per target, which is CMake's word for the thing.
    #[serde(default, rename = "target")]
    pub targets: Vec<TargetSection>,
}

/// **One compilation target** — what a build system has and one `[compile]` cannot say.
///
/// ```toml
/// [compile]
/// standard = "c++20"
/// fuzzy_includes = true
///
/// [[compile.target]]
/// name     = "CodeFormatServer"
/// sources  = ["CodeFormatServer/**"]
/// includes = ["CodeFormatServer/include", "CodeFormatServer/src", "Util/include"]
/// defines  = ["URI_STATIC_BUILD", "WIN32_LEAN_AND_MEAN"]
/// ```
///
/// # The syntax, and how much of CMake it is
///
/// Deliberately a few ideas and no more — CMake's own words where they fit, so that a person who knows
/// `target_include_directories` and `target_compile_definitions` has nothing to learn:
///
/// ```text
/// name        what to call it in a message        (CMake: the target's name)
/// sources     which files it describes           (CMake: add_executable's list, as a pattern)
/// includes    its include directories            (CMake: target_include_directories)
/// defines     its preprocessor definitions       (CMake: target_compile_definitions)
/// standard    its language standard              (CMake: CXX_STANDARD)
/// ```
///
/// **What it is not** is a build system: there are no dependencies, no sources to compile, no commands to run,
/// and nothing here can make anything happen. It describes compilations, which is all an analysis needs and all a
/// person should have to write down. Anything that must be *generated* belongs in a compile database — which is
/// what `[compile].database` is for.
///
/// # How a file finds its target
///
/// `sources` is a **pattern**, matched against the file's path relative to the root, component by component:
///
/// ```text
/// "CodeFormatServer/**"     the subtree, at any depth
/// "*/src/**"                any target's src tree
/// "Util/*.cpp"              direct children only
/// ```
///
/// `**` stands for any number of components (including none), `*` for any run of characters **within** one, and
/// everything else is literal. A pattern is matched against the whole path unless its last component is `**`.
///
/// When several targets match, the **most specific** wins — the one fixing the most literal components — and ties
/// go to the one written first. A target for `Util/**` therefore beats one for `**`, and which target describes a
/// file is decided by the file rather than by the order of the tables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSection {
    /// What to call this target in a problem message. Absent is allowed; [`TargetSection::label`] then says
    /// something a person can still act on.
    #[serde(default)]
    pub name: Option<String>,
    /// Which files this target describes, as patterns relative to the project root.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Its include directories, relative to the project root (or absolute).
    #[serde(default)]
    pub includes: Vec<String>,
    /// Its preprocessor definitions — `"NAME"` or `"NAME=value"`, without the `-D`.
    #[serde(default)]
    pub defines: Vec<String>,
    /// Its language standard, as `-std=` spells it (`"c++20"`). The project's is used when this is absent.
    #[serde(default)]
    pub standard: Option<String>,
    /// The target's **whole** command line, when a person wants to write it out — used *instead of* the
    /// discovered flags for the files it describes, exactly as [`CompileSection::args`] is for the project.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Flags added on top of whichever set won for this target.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Flags dropped from whichever set won for this target.
    #[serde(default)]
    pub remove_args: Vec<String>,
}

/// **The target that describes `file`**, for a caller that holds the list and not the section.
///
/// The same rule [`CompileSection::target_for`] applies, as a function, because the include resolver is built per
/// file from a [`CompilerConfig`](crate::CompilerConfig) and never sees the configuration file itself.
/// **Every target a configuration describes**, including the one `[compile.include]` implies.
///
/// `[compile].include` is the older, smaller spelling of the same idea — a directory and the include paths it
/// needs — and it is turned into a target here rather than read anywhere else, so that there is **one** mechanism
/// deciding which include directories a file gets. Before this it was parsed, validated, documented and never
/// applied: a project that wrote it got nothing, which is one reason a whole project's `-I` flags end up in a
/// single `args` list.
///
/// A key of `"*"` becomes the target for every file; any other key becomes that subtree's.
pub fn targets_for(section: &CompileSection) -> Vec<TargetSection> {
    let mut targets: Vec<TargetSection> = section
        .include
        .iter()
        .map(|(directory, includes)| TargetSection {
            name: Some(format!("[compile.include] {directory}")),
            sources: vec![if directory == "*" {
                "**".to_string()
            } else {
                format!("{directory}/**")
            }],
            includes: includes.clone(),
            ..TargetSection::default()
        })
        .collect();

    targets.extend(section.targets.iter().cloned());
    targets
}
pub fn target_for<'a>(
    targets: &'a [TargetSection],
    root: &Path,
    file: &Path,
) -> Option<&'a TargetSection> {
    targets
        .iter()
        .enumerate()
        .filter_map(|(order, target)| {
            target
                .describes(root, file)
                .map(|specific| (specific, std::cmp::Reverse(order), target))
        })
        .max_by_key(|(specific, order, _)| (*specific, *order))
        .map(|(_, _, target)| target)
}
impl TargetSection {
    /// What to call it when a message has to name it.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("an unnamed target")
    }

    /// **How specifically this target describes `file`**, or `None` when it does not.
    ///
    /// The number is the count of **literal** components the pattern fixed, which is what "most specific" means to
    /// a reader: `Util/src/**` fixes two, `**` fixes none. No `sources` at all matches nothing — a target that
    /// names no files is not a target for every file.
    pub fn describes(&self, root: &Path, file: &Path) -> Option<usize> {
        let relative = file.strip_prefix(root).ok()?;
        let components: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect();

        self.sources
            .iter()
            .filter_map(|pattern| pattern_specificity(pattern, &components))
            .max()
    }
}

/// **Does `pattern` describe the path whose components are `path`**, and how specifically?
///
/// `Some(literal components fixed)` when it does, `None` when it does not. Matching is case-insensitive because a
/// path that differs only in case is the same file on the platforms this runs on, and a configuration file should
/// not be what decides otherwise.
fn pattern_specificity(pattern: &str, path: &[String]) -> Option<usize> {
    let parts: Vec<String> = pattern
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| part.to_lowercase())
        .collect();

    if parts.is_empty() {
        return None;
    }

    /// `**` matches any number of components, including none, so the walk carries how many literal components
    /// have been fixed and takes the best of every way through.
    fn walk(parts: &[String], path: &[String], fixed: usize) -> Option<usize> {
        let Some((head, rest)) = parts.split_first() else {
            // The pattern is spent: it describes this file only if the path is spent too.
            return path.is_empty().then_some(fixed);
        };

        if head == "**" {
            return (0..=path.len())
                .filter_map(|skipped| walk(rest, &path[skipped..], fixed))
                .max();
        }

        let Some((component, remaining)) = path.split_first() else {
            return None;
        };
        if !component_matches(head, component) {
            return None;
        }

        let fixed = if head.contains('*') { fixed } else { fixed + 1 };
        walk(rest, remaining, fixed)
    }

    walk(&parts, path, 0)
}

/// One component of a pattern against one component of a path: `*` is any run of characters.
fn component_matches(pattern: &str, component: &str) -> bool {
    let Some(star) = pattern.find('*') else {
        return pattern == component;
    };

    let (before, after) = (&pattern[..star], &pattern[star + 1..]);
    component.starts_with(before)
        && component.len() >= before.len() + after.len()
        && component.ends_with(after)
}

impl CompileSection {
    /// Is the compiler left to discovery?
    pub fn compiler_is_auto(&self) -> bool {
        match &self.compiler {
            None => true,
            Some(named) => named.eq_ignore_ascii_case("auto"),
        }
    }

    /// The compiler this file names, when it names one.
    pub fn named_compiler(&self) -> Option<&str> {
        match self.compiler.as_deref() {
            Some(named) if !self.compiler_is_auto() => Some(named),
            _ => None,
        }
    }

    /// **The target that describes `file`**, when one does.
    ///
    /// The most specific match wins and ties go to the one written first — see [`TargetSection::describes`] for
    /// what "specific" counts. A file no target matches is described by `[compile]` itself, which is what makes
    /// targets an addition rather than a replacement.
    pub fn target_for(&self, root: &Path, file: &Path) -> Option<&TargetSection> {
        self.targets
            .iter()
            .enumerate()
            .filter_map(|(order, target)| {
                target
                    .describes(root, file)
                    .map(|specific| (specific, std::cmp::Reverse(order), target))
            })
            .max_by_key(|(specific, order, _)| (*specific, *order))
            .map(|(_, _, target)| target)
    }
    /// The extra include directories that apply to a file.
    ///
    /// Longest matching prefix wins, and `"*"` applies everywhere: a project that says `"src/legacy"` means that
    /// subtree and not the project, and one that says `"*"` means all of it. Both can be true at once, and the
    /// nearer rule is the one that should be added first — the resolver searches include paths in order.
    pub fn includes_for(&self, root: &Path, file: &Path) -> Vec<PathBuf> {
        let mut chosen: Vec<(&str, &Vec<String>)> = self
            .include
            .iter()
            .filter(|(directory, _)| {
                directory.as_str() == "*" || file.starts_with(root.join(directory))
            })
            .map(|(directory, paths)| (directory.as_str(), paths))
            .collect();

        // The specific directories first, then `*`; longest prefix first among the specific ones.
        chosen
            .sort_by_key(|(directory, _)| (directory == &"*", std::cmp::Reverse(directory.len())));

        chosen
            .into_iter()
            .flat_map(|(_, paths)| paths.iter())
            .map(|directory| root.join(directory))
            .collect()
    }
}

/// `[index]` — the cache and the scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSection {
    /// The cache directory's name, relative to the workspace root.
    #[serde(default)]
    pub cache_dir: Option<String>,
    /// How many files the project scan may list.
    #[serde(default)]
    pub max_files: Option<usize>,
}

/// `[diagnostics]` — read by the language server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsSection {
    /// How long a file's diagnostics wait for the typing to stop, in milliseconds.
    #[serde(default)]
    pub on_change_ms: Option<u64>,
}

/// `[hover]` — read by the language server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoverSection {
    #[serde(default)]
    pub enable: Option<bool>,
}

/// How bad a configuration problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The file could not be read as TOML, or a section could not be decoded: what that section said is **not**
    /// in effect.
    Error,
    /// The file said something that is understood but unusable — a cache directory that is not a directory name, a
    /// pattern that is not a pattern. The rest of the configuration is in effect.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    pub severity: Severity,
    /// One sentence, in the shape a person can act on: what was wrong, and what was done instead.
    pub message: String,
}

impl ConfigProblem {
    fn error(message: impl Into<String>) -> Self {
        ConfigProblem {
            severity: Severity::Error,
            message: message.into(),
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        ConfigProblem {
            severity: Severity::Warning,
            message: message.into(),
        }
    }
}

/// A configuration file, read: what it said, where it was, and everything wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigReport {
    pub config: ProjectConfig,
    /// The file this came from, when there was one.
    pub path: Option<PathBuf>,
    /// Every problem, in the order they were found. Empty is the ordinary case.
    pub problems: Vec<ConfigProblem>,
}

impl ConfigReport {
    /// The configuration a project with no file has: everything default, nothing wrong.
    pub fn none() -> Self {
        ConfigReport::default()
    }

    /// Was the file read at all?
    pub fn found(&self) -> bool {
        self.path.is_some()
    }

    /// The problems worth showing a user without being asked.
    pub fn errors(&self) -> impl Iterator<Item = &ConfigProblem> {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
    }
}

/// Read a project's configuration from a provider.
///
/// `explicit` is a path a caller named (a command-line `--config`); when it is `None` the conventional name in the
/// workspace root is tried. A file that is not there is **not** a problem: every project without a configuration
/// file is the ordinary case, and a report with no path says so.
pub fn load_config(
    files: &impl FileProvider,
    root: &Path,
    explicit: Option<&Path>,
) -> ConfigReport {
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => root.join(PROJECT_CONFIG_FILE),
    };

    let Some(text) = files.read(&path) else {
        if explicit.is_some() {
            return ConfigReport {
                config: ProjectConfig::default(),
                path: None,
                problems: vec![ConfigProblem::error(format!(
                    "the configuration file {} could not be read",
                    path.display()
                ))],
            };
        }

        return ConfigReport::none();
    };

    parse_config(&text, &path)
}

/// Read a configuration file's text.
///
/// Split from [`load_config`] so that the parse can be tested without a filesystem — and because the text is the
/// only input that decides what the configuration is: a test that goes through a `MemoryFiles` asserts the same
/// thing with more moving parts.
pub fn parse_config(text: &str, path: &Path) -> ConfigReport {
    let value: toml::Value = match toml::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return ConfigReport {
                config: ProjectConfig::default(),
                path: Some(path.to_path_buf()),
                problems: vec![ConfigProblem::error(format!(
                    "{} is not valid TOML, so none of it is in effect: {error}",
                    path.display()
                ))],
            };
        }
    };

    let mut report = ConfigReport {
        config: ProjectConfig::default(),
        path: Some(path.to_path_buf()),
        problems: Vec::new(),
    };

    let Some(table) = value.as_table() else {
        report.problems.push(ConfigProblem::error(format!(
            "{} does not hold a table of sections",
            path.display()
        )));
        return report;
    };

    // Each section on its own, so that one typo cannot take the others with it.
    for (name, section) in table {
        let section = section.clone();

        match name.as_str() {
            "workspace" => match decode::<WorkspaceSection>(section, name) {
                Ok(decoded) => report.config.workspace = decoded,
                Err(problem) => report.problems.push(problem),
            },
            "compile" => match decode::<CompileSection>(section, name) {
                Ok(decoded) => report.config.compile = decoded,
                Err(problem) => report.problems.push(problem),
            },
            "index" => match decode::<IndexSection>(section, name) {
                Ok(decoded) => report.config.index = decoded,
                Err(problem) => report.problems.push(problem),
            },
            "diagnostics" => match decode::<DiagnosticsSection>(section, name) {
                Ok(decoded) => report.config.diagnostics = decoded,
                Err(problem) => report.problems.push(problem),
            },
            "hover" => match decode::<HoverSection>(section, name) {
                Ok(decoded) => report.config.hover = decoded,
                Err(problem) => report.problems.push(problem),
            },
            unknown => report.problems.push(ConfigProblem::error(format!(
                "[{unknown}] is not a section this server reads (workspace, compile, index, diagnostics, hover), \
                 so nothing under it is in effect"
            ))),
        }
    }

    validate(&mut report);
    report
}

/// Decode one section, reporting a failure instead of failing the file.
fn decode<T>(value: toml::Value, name: &str) -> Result<T, ConfigProblem>
where
    T: for<'de> Deserialize<'de>,
{
    value.try_into::<T>().map_err(|error| {
        ConfigProblem::error(format!(
            "[{name}] could not be read, so that section keeps its defaults: {error}"
        ))
    })
}

/// The checks serde cannot make: values that are the right *type* and still unusable.
fn validate(report: &mut ConfigReport) {
    if let Some(cache_dir) = &report.config.index.cache_dir {
        let path = Path::new(cache_dir);
        let one_component = path.components().count() == 1;
        let named = matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        );

        if !one_component || !named {
            report.problems.push(ConfigProblem::warning(format!(
                "index.cache_dir = {cache_dir:?} is not a directory name inside the project, so the default \
                 `{}` is used instead",
                crate::CACHE_DIRECTORY
            )));
            report.config.index.cache_dir = None;
        }
    }

    if report.config.index.max_files == Some(0) {
        report.problems.push(ConfigProblem::warning(
            "index.max_files = 0 would mean \"index nothing\", so the default is used instead"
                .to_string(),
        ));
        report.config.index.max_files = None;
    }

    // The patterns are compiled by the caller that builds the filter, but a bad one is a *configuration* problem
    // and belongs in the report the user reads — so it is checked here, once, where the file is read.
    let bad_patterns: Vec<&String> = report
        .config
        .workspace
        .exclude
        .iter()
        .filter(|pattern| crate::PathPattern::new(pattern).is_err())
        .collect();

    for pattern in bad_patterns {
        report.problems.push(ConfigProblem::warning(format!(
            "workspace.exclude has {pattern:?}, which is not a path pattern, so it is ignored"
        )));
    }
    report
        .config
        .workspace
        .exclude
        .retain(|pattern| crate::PathPattern::new(pattern).is_ok());

    for relative in report.config.compile.include.values().flatten() {
        if Path::new(relative).is_absolute() {
            report.problems.push(ConfigProblem::warning(format!(
                "compile.include names the absolute path {relative:?}; absolute paths in a committed file do not \
                 survive a move, and this one is used as written"
            )));
        }
    }

    if report
        .config
        .compile
        .database
        .as_ref()
        .is_some_and(|database| {
            Path::new(database)
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        })
    {
        report.problems.push(ConfigProblem::warning(
            "compile.database walks out of the project with `..`; it is used as written"
                .to_string(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ConfigReport {
        parse_config(text, Path::new("/p/.cppls.toml"))
    }

    fn problems(report: &ConfigReport) -> Vec<String> {
        report
            .problems
            .iter()
            .map(|problem| problem.message.clone())
            .collect()
    }

    #[test]
    fn an_empty_file_is_the_default_configuration() {
        let report = parse("");

        assert_eq!(report.config, ProjectConfig::default());
        assert!(report.problems.is_empty());
        assert!(report.found());
    }

    #[test]
    fn every_key_is_read() {
        let report = parse(
            r#"
            [workspace]
            exclude = ["**/build/**", "third_party/**"]
            source_extensions = ["cuh"]

            [compile]
            database = "build/compile_commands.json"
            compiler = "clang++"
            args = ["-std=c++20", "-Iinclude"]
            extra_args = ["-DFEATURE=1"]
            remove_args = ["-Werror"]
            [compile.include]
            "src/legacy" = ["include/legacy"]

            [index]
            cache_dir = ".cache"
            max_files = 20000

            [diagnostics]
            on_change_ms = 250

            [hover]
            enable = false
            "#,
        );

        assert!(report.problems.is_empty(), "{:?}", problems(&report));
        let config = &report.config;
        assert_eq!(config.workspace.exclude, ["**/build/**", "third_party/**"]);
        assert_eq!(config.workspace.source_extensions, ["cuh"]);
        assert_eq!(
            config.compile.database.as_deref(),
            Some(Path::new("build/compile_commands.json"))
        );
        assert_eq!(config.compile.named_compiler(), Some("clang++"));
        assert_eq!(
            config.compile.args.as_deref(),
            Some(["-std=c++20".to_string(), "-Iinclude".to_string()].as_slice())
        );
        assert_eq!(config.compile.extra_args, ["-DFEATURE=1"]);
        assert_eq!(config.compile.remove_args, ["-Werror"]);
        assert_eq!(
            config.compile.include.get("src/legacy").map(Vec::len),
            Some(1)
        );
        assert_eq!(config.index.cache_dir.as_deref(), Some(".cache"));
        assert_eq!(config.index.max_files, Some(20000));
        assert_eq!(config.diagnostics.on_change_ms, Some(250));
        assert_eq!(config.hover.enable, Some(false));
    }

    #[test]
    fn auto_is_not_a_compiler() {
        let report = parse("[compile]\ncompiler = \"auto\"\n");

        assert!(report.config.compile.compiler_is_auto());
        assert_eq!(report.config.compile.named_compiler(), None);
    }

    #[test]
    fn an_unknown_section_does_not_take_the_others_with_it() {
        // The typo that would otherwise be the most silent of all: a misspelled section header.
        let report =
            parse("[workspace]\nexclude = [\"build/**\"]\n\n[compilee]\ncompiler = \"clang++\"\n");

        assert_eq!(report.config.workspace.exclude, ["build/**"]);
        assert_eq!(report.config.compile.named_compiler(), None);
        assert_eq!(report.problems.len(), 1);
        assert!(
            report.problems[0].message.contains("[compilee]"),
            "{:?}",
            problems(&report)
        );
        assert_eq!(report.problems[0].severity, Severity::Error);
    }

    #[test]
    fn an_unknown_key_in_a_section_keeps_that_section_at_its_defaults_and_says_which() {
        let report = parse("[workspace]\nexlude = [\"build/**\"]\n");

        assert!(report.config.workspace.exclude.is_empty());
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].severity, Severity::Error);
        assert!(
            report.problems[0].message.contains("exlude"),
            "the problem names the key: {:?}",
            problems(&report)
        );
    }

    #[test]
    fn a_section_of_the_wrong_shape_is_reported() {
        let report = parse("[index]\ncache_dir = 5\n");

        assert_eq!(report.config.index.cache_dir, None);
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].message.contains("[index]"));
    }

    #[test]
    fn a_syntax_error_reports_the_file_and_keeps_nothing() {
        let report = parse("[workspace\nexclude = [\n");

        assert_eq!(report.config, ProjectConfig::default());
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].message.contains("not valid TOML"));
    }

    #[test]
    fn a_cache_directory_that_is_not_a_name_is_replaced_by_the_default() {
        for written in ["../outside", "/tmp/cppls", "a/b"] {
            let report = parse(&format!("[index]\ncache_dir = {written:?}\n"));

            assert_eq!(report.config.index.cache_dir, None, "{written}");
            assert_eq!(report.problems.len(), 1, "{written}");
            assert_eq!(report.problems[0].severity, Severity::Warning);
        }
    }

    #[test]
    fn a_pattern_that_is_not_a_pattern_is_dropped_from_the_list() {
        let report = parse("[workspace]\nexclude = [\"**/build/**\", \"**/[unclosed\"]\n");

        assert_eq!(report.config.workspace.exclude, ["**/build/**"]);
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].severity, Severity::Warning);
        assert!(report.problems[0].message.contains("unclosed"));
    }

    #[test]
    fn an_include_a_project_names_is_relative_to_the_root_and_the_nearest_rule_wins() {
        let report = parse(
            r#"
            [compile.include]
            "*" = ["include"]
            "src/legacy" = ["include/legacy", "generated"]
            "#,
        );
        let root = Path::new("/p");

        let legacy = report
            .config
            .compile
            .includes_for(root, Path::new("/p/src/legacy/old.cpp"));
        assert_eq!(
            legacy,
            [
                PathBuf::from("/p/include/legacy"),
                PathBuf::from("/p/generated"),
                PathBuf::from("/p/include"),
            ],
            "the specific directory first, then the project-wide one"
        );

        let elsewhere = report
            .config
            .compile
            .includes_for(root, Path::new("/p/src/new/modern.cpp"));
        assert_eq!(elsewhere, [PathBuf::from("/p/include")]);
    }

    #[test]
    fn a_file_that_is_not_there_is_not_a_problem() {
        let files = crate::MemoryFiles::new();
        let report = load_config(&files, Path::new("/p"), None);

        assert!(!report.found());
        assert!(report.problems.is_empty());
    }

    #[test]
    fn a_file_the_caller_named_must_exist() {
        // `--config x` is an instruction, not a convention: a missing one is worth saying out loud.
        let files = crate::MemoryFiles::new();
        let report = load_config(&files, Path::new("/p"), Some(Path::new("/p/other.toml")));

        assert!(!report.found());
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0].message.contains("other.toml"));
    }

    #[test]
    fn a_configuration_is_read_from_a_provider() {
        let files = crate::MemoryFiles::new()
            .with_file("/p/.cppls.toml", "[workspace]\nexclude = [\"build/**\"]\n");
        let report = load_config(&files, Path::new("/p"), None);

        assert_eq!(report.path.as_deref(), Some(Path::new("/p/.cppls.toml")));
        assert_eq!(report.config.workspace.exclude, ["build/**"]);
    }
}
