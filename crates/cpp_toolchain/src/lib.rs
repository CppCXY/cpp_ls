//! **The toolchain and the preprocessor** — how a translation unit is found, configured, walked and rendered.
//!
//! # Why this crate exists
//!
//! The analysis answers questions about *code*. Before it can, two things have to happen: the project has to be
//! described (which compiler, which include paths, which files are its own), and a file has to be read the way a
//! compiler reads it — directives applied, macros expanded, comments gone. Both are filesystem and compiler
//! questions, and neither needs to know what a declaration *means*.
//!
//! ```text
//! project    `.cppls.toml`, `compile_commands.json`, CMake, the discovered toolchain
//! include    where `#include "x.h"` and `#include <x.h>` actually look
//! walk       the translation unit: which file includes which, and which macros are in force where
//! cook       one file's text as the compiler sees it: the rendering, and the spans back to the file
//! preprocess directives, macros, conditions, guards, expansion
//! paths      the files a reader's questions are answered from
//! ```
//!
//! # The one rule this crate enforces by existing
//!
//! The dependency direction is one-way: `cpp_code_analysis` depends on this crate, and this crate may not depend on
//! it. What that forbids is what the split exists for — **the preprocessor may not ask the index what a name means.**
//! A question that needs the index is a question for the layer above; this layer answers what the compiler's own
//! inputs say, and says `Unknown` when they do not say it.
//!
//! # The seam the analysis is rebuilt around
//!
//! A *file's rendering* is this crate's unit of work: lex, apply the environment, expand, and keep the spans back to
//! the file. That unit is what the new cache keys and stores, which is why it lives here rather than inside the
//! session. [`preprocess::cooked::FileMacros`] is the one place the two layers meet: it asks for a file's macro
//! environment and a unit's definitions through two small traits ([`preprocess::cooked::MacroEnvironment`],
//! [`preprocess::cooked::UnitDefinitionsView`]), so the walk's timeline stays in the analysis and the renderer stays
//! here.

// **Two lints this crate answers with a rationale rather than a change.**
//
// `too_many_arguments`: a question in this layer is asked with the compiler's context — the provider, the runner, the
// name, the language, the flags — and then with what the question is *about*. The largest is
// [`include::toolchain::ask`] at nine, and it is one step of the discovery stack rather than a public API: bundling
// the context into a struct would rebuild it at every call to re-borrow the same fields.
//
// `type_complexity`: the memo tables are spelled out where they are declared, because the shape is the answer to
// "what have I already been asked" and naming it would move that shape away from the code that inserts into it.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

// **The diagnostics speak the reader's language.** The keys are the English templates themselves, so `en` needs no
// file — a missing key answers with the key, which *is* the English message. The analysis crate loads the same table
// (`cpp_parser::set_locale` switches the process-wide locale), and the two files are the same vocabulary split at
// the crate boundary; see `locales/zh-CN.yml`. The macro this defines is `t!`, and it is generated here rather than
// imported: `i18n!` is what registers the table the macro reads.
rust_i18n::i18n!("./locales", fallback = "en");

// The folders are the organisation; the modules inside them keep their own names and are re-exported at the root, so
// that a path written inside this crate (`crate::macros::MacroDef`, `crate::config::CompilerConfig`) names the same
// item a consumer sees. Moving a file is a change to the filesystem, and this is what keeps it from being a change to
// the vocabulary.
pub mod hash;
pub mod include;
pub mod paths;
pub mod preprocess;
pub mod project;
pub mod token;

/// **What a file layer is called from above** — `crate::file::paths` is the spelling the analysis was written with,
/// and this keeps it resolving while the layer it names lives here.
pub mod file {
    pub use crate::paths;
}

pub use include::{config, fuzzy, graph, msvc, system_headers, toolchain};
pub use preprocess::{condition, cooked, directive, expand, guard, guards, macros};

pub use hash::{content_hash, fnv1a64};
pub use paths::{
    CACHE_DIRECTORY, CachedFiles, DiskFiles, FileId, FileProvider, MemoryFiles, OverlayFiles,
    PathInterner, join_normalized, normalize_path, parent_normalized,
};
pub use token::Token;

pub use condition::{ConditionExpr, EvalError, Lookup, MacroValues, NoMacros, Value};
pub use config::{
    CommandLineMacro, CompileCommand, CompileCommands, CompilerConfig, IncludePath,
    ProjectLanguage, config_from_arguments, parse_compile_commands, predefined_macros_of,
    project_config_from_flags, split_command_line,
};
pub use directive::{
    Define, Directive, DirectiveKind, Include, IncludeForm, SpannedDirective,
    parse_directive_tokens, scan_directives,
};
pub use expand::{
    Diagnostic, ExpandedToken, Expansion, ExpansionNote, MacroInvocation, Origin, expand,
    expand_with_budget,
};
pub use graph::{
    Edge, FileEntry, FileGraph, MAX_INCLUDE_DEPTH, Marked, SkipReason, UnresolvedInclude, Visit,
    WalkScope, file_only, walk, walk_scoped,
};
pub use guard::{Branch, Guard, GuardStack, Region, Visibility};
pub use guards::{FileGuard, GuardAnalysis, analyse_guards, detect_guard};
pub use include::feature::{CompilerFeatureProbe, FeatureCache, FeatureProbe, ProbeLanguage};
pub use include::{FoundIn, IncludeResolver, Resolution, Resolved, Unresolved};
pub use macros::{
    MacroBindings, MacroBody, MacroDef, MacroTable, Parameter, ParameterKind, UnitSources,
};
pub use project::{
    CONFIGURATION_FILE_NAMES, CompileSection, ConfigProblem, ConfigReport, DEFAULT_FORMATTER,
    Database, DatabaseOrigin, DiagnosticsSection, DiscoveryProblem, FormatStringsSection,
    FormatSyntax, FormattingSection, FunctionEntry, HoverSection, IndexSection,
    PROJECT_CONFIG_FILE, ProjectConfig, ProjectDiscovery, Severity, WorkspaceSection,
    find_database, load_config, parse_config,
};
pub use toolchain::{
    CommandRunner, DiskCommands, Environment, Output, Toolchain, ToolchainSource, discover,
    discover_with, find_compiler, parse_search_list, parse_version, search_paths,
};
