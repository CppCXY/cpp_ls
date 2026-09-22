//! Code analysis on top of the C++ syntax tree.
//!
//! # What this crate is for
//!
//! `cpp_parser` answers "what does this file *look* like" — a lossless tree of every token, with no
//! opinion about what any of it means. This crate answers "what does it *mean*", which in C++ starts
//! with the preprocessor: the text a compiler sees is not the text in the file.
//!
//! # Why the preprocessor is a separate layer over the tree, not a pass before it
//!
//! The obvious design — run a preprocessor, then parse its output — is the one a compiler uses, and it
//! is unusable for an editor:
//!
//! * **The tree must stay lossless.** Expansion results cannot be written back into the tree without
//!   losing the correspondence between a byte of source and a byte of the file. So expansion produces
//!   a *shadow* token stream and the tree is left alone.
//! * **Both branches of `#if` must stay visible.** A compiler picks one; an editor cannot. Code inside
//!   `#if defined(_WIN32)` is code a person is writing, on whatever platform they happen to be on, and
//!   making it disappear is the failure mode users notice first.
//! * **Every edit would invalidate everything.** Expansion is not local — one `#define` changes the
//!   meaning of every file that includes it — so doing it before parsing would make the parse itself
//!   un-incremental.
//!
//! # What this layer produces
//!
//! ```text
//! PreprocessorDirective nodes  ->  typed Directive values        (directive.rs)
//! #define                      ->  MacroDef, token sequence       (macros.rs)
//! #if / #elif                  ->  Guard conditions over macros   (condition.rs, guard.rs)
//! a macro call                 ->  a shadow token stream, every
//!                                 token carrying its origin       (expand.rs)
//! ```
//!
//! A [`Guard`] travels with everything derived from a conditional region, so that a consumer can ask
//! "is this code even being compiled?" and get `Active`, `Inactive`, or — the common answer —
//! `Unknown`, because `#if _MSC_VER > 1900` cannot be decided on a machine that is not MSVC.
//!
//! An [`ExpandedToken`] travels with a [`Origin`], so that "where is this?" has an answer even when the
//! token was written in a different file: navigation reaches the macro, diagnostics reach the call site.

pub mod condition;
pub mod config;
pub mod directive;
pub mod expand;
pub mod file;
pub mod graph;
pub mod guard;
pub mod guards;
pub mod include;
pub mod macros;
pub mod module_info;
pub mod modules;
pub mod paths;
pub mod preprocess;
pub mod token;

pub use condition::{ConditionExpr, EvalError, MacroValues, Value, evaluate, parse_condition};
pub use config::{
    CommandLineMacro, CompileCommand, CompileCommands, CompilerConfig, IncludePath,
    parse_compile_commands, split_command_line,
};
pub use directive::{
    Define, Directive, DirectiveKind, Include, IncludeForm, SpannedDirective,
    parse_directive_tokens, scan_directives,
};
pub use expand::{
    Diagnostic, ExpandedToken, Expansion, ExpansionNote, MacroInvocation, Origin, expand,
    expand_with_budget,
};
pub use file::{FileAnalysis, FileTokens};
pub use graph::{
    Edge, FileEntry, FileGraph, MAX_INCLUDE_DEPTH, SkipReason, UnresolvedInclude, Visit, WalkScope,
    file_only, walk, walk_scoped,
};
pub use guard::{Branch, Guard, GuardStack, Region, Visibility};
pub use guards::{FileGuard, GuardAnalysis, analyse_guards, detect_guard};
pub use include::{FoundIn, IncludeResolver, Resolution, Resolved, Unresolved};
pub use macros::{MacroBody, MacroDef, MacroTable, Parameter, ParameterKind};
pub use module_info::{ImportDeclaration, ImportTarget, ModuleInfo, ModuleUnit};
pub use modules::{
    ImportEdge, ImportOutcome, MAX_IMPORT_DEPTH, ModuleGraph, ModuleScanner, ModuleUnitEntry,
    scan_imports,
};
pub use paths::{
    DiskFiles, FileId, FileProvider, MemoryFiles, OverlayFiles, PathInterner, join_normalized,
    normalize_path, parent_normalized,
};
pub use preprocess::{FilePreprocessing, PositionalMacros, preprocess};
pub use token::Token;
