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
    /// (`docs/grammar-gaps.md` B61) — so the parser has to be told which compiler is reading the file.
    ///
    /// Set from the toolchain's own predefined macros (`__GNUC__` / `_MSC_VER`, see
    /// [`Dialect::from_predefined_macros`]) rather than guessed from the flags, and it is part of the summary's
    /// **context hash**: the same text read for two targets is two different summaries, and a cache that confused
    /// them would serve one target's facts to the other.
    pub dialect: Dialect,

    /// The directory the compilation was run in.
    ///
    /// A relative `-I` and a relative `#include` are both relative to *this*, not to the file or to the
    /// editor's working directory. Without it, a project's own include paths resolve against nothing in
    /// particular.
    pub working_directory: Option<PathBuf>,
}

impl CompilerConfig {
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

    /// Parse for a **target compiler**: what its reserved spellings mean. See [`CompilerConfig::dialect`].
    pub fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// The dialect in force. [`Dialect::default`] until a toolchain says otherwise.
    pub fn dialect(&self) -> Dialect {
        self.dialect
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
    /// The file this command compiles.
    pub file: PathBuf,
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
        let mut config = CompilerConfig {
            working_directory: self.directory.clone(),
            ..CompilerConfig::default()
        };

        let mut index = 0;
        while index < self.arguments.len() {
            let argument = self.arguments[index].as_str();
            index += 1;

            // The two spellings of every option: joined (`-Ifoo`) and separate (`-I foo`).
            let take_value = |index: &mut usize| -> Option<&str> {
                let value = self.arguments.get(*index).map(String::as_str);
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
}

/// Read `-DFOO` or `-DFOO=bar`.
///
/// `-DFOO` defines `FOO` as `1` — the standard says so, and `#if FOO` depends on it. An empty value
/// (`-DFOO=`) is *not* the same thing: it is a definition with an empty body, which `#if FOO` rejects.
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

/// A parsed compile database, and what was skipped.
#[derive(Debug, Clone, Default)]
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
        let requested = normalize_for_comparison(file);
        let request_tails: std::collections::HashSet<&str> =
            path_suffixes(&requested).into_iter().collect();

        // The most specific match wins, so a database with both `/a/x.h` and `/b/a/x.h` answers a request
        // for the latter with the entry that names more of it.
        let mut best: Option<(usize, &CompileCommand)> = None;

        for command in &self.commands {
            let stored = normalize_for_comparison(&command.file);

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
fn normalize_for_comparison(path: &Path) -> String {
    let unified: String = path
        .to_string_lossy()
        .chars()
        .map(|character| if character == '\\' { '/' } else { character })
        .collect();

    crate::paths::normalize_path(Path::new(&unified), true)
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
fn parse_command_object(object: &str) -> Option<CompileCommand> {
    let directory = string_field(object, "directory").map(PathBuf::from);
    let file = string_field(object, "file")?;

    // Either shape: `arguments` is a list, `command` is a shell string. Both are in the wild, and a
    // project may use either, so neither can be treated as the real one.
    let arguments = match array_field(object, "arguments") {
        Some(arguments) => arguments,
        None => split_command_line(&string_field(object, "command")?),
    };

    Some(CompileCommand {
        file: PathBuf::from(file),
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
pub fn split_command_line(command: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            has_current = true;
            escaped = false;
            continue;
        }

        match character {
            '\\' if quote != Some('\'') => escaped = true,
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
