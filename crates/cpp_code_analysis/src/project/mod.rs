//! # What this project is, and how it is built
//!
//! One question, asked once per workspace and again whenever something it depends on changes: **how should this
//! project be analysed?** The answer is assembled from four sources, each weaker than the one before it, and the
//! assembled answer says which source each part came from:
//!
//! ```text
//! .cppls.toml              a person said so — the only source that can override everything else
//! compile_commands.json    the project's own build, as the build system ran it
//! CMakeCache.txt           what CMake was configured with, when it did not export a database
//! the toolchain            the compiler is asked where its headers are, and what it predefines
//! the system's headers     a last resort: known layouts, labelled as a guess
//! ```
//!
//! # Why this is a layer of its own
//!
//! Because "which flags, which headers, which files" is one decision with four inputs, and the previous arrangement
//! had it spread across three places (the session read the database and asked the toolchain; the language server
//! read the client's settings; nothing read the project's own file). The failure mode of that arrangement is
//! specific and quiet: two layers disagree about what the project *is*, and the analysis answers confidently about
//! a compilation that does not exist.
//!
//! # What it produces
//!
//! A [`ProjectConfig`] — what the file said — and, once discovery is complete, a report of what was found, where it
//! was found, and everything that could not be worked out. Both are part of the session's public state, because the
//! first question anybody asks about a wrong answer is "what did it think this project was".
//!
//! # The rule that shapes all of it
//!
//! **A guess is allowed; a guess pretending to be a fact is not.** Every include path, every macro environment and
//! every compiler carries where it came from, and the layer above can tell a compiler's answer from a convention's.
//! When nothing can be worked out, the analysis says so — conditions stay `Unknown` and the include stays
//! unresolved — rather than inventing a configuration that makes the numbers look better.

pub mod build;
pub mod config;
pub mod discovery;

pub use build::{
    CmakeCache, COMPILE_DATABASE_FILE, CMAKE_CACHE_FILE, Database, DatabaseOrigin, find_cmake_cache,
    find_database, parse_cmake_cache,
};
pub use config::{
    CompileSection, ConfigProblem, ConfigReport, DiagnosticsSection, HoverSection, IndexSection,
    PROJECT_CONFIG_FILE, ProjectConfig, Severity, WorkspaceSection, load_config, parse_config,
};
pub use discovery::{DiscoveryProblem, ProjectDiscovery, discover};

/// The file names that **configure** an analysis rather than being part of it.
///
/// A change to one of these is not an edit to index — it is a reason to work the project out again — so they are
/// recognised by name anywhere in the tree: `compile_commands.json` in a build directory says as much about how
/// this project is built as one in the root, and `CMakeCache.txt` is CMake's own record of the configuration that
/// produced it. The list is here, next to the file that is read, so that the watcher and the discovery cannot
/// disagree about which files matter.
pub const CONFIGURATION_FILE_NAMES: &[&str] = &[
    PROJECT_CONFIG_FILE,
    "compile_commands.json",
    "CMakeCache.txt",
];
