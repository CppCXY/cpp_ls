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

pub mod cache;
pub mod file;
pub mod include;
pub mod index;
pub mod preprocess;
pub mod sema;
pub mod session;
pub mod summary;
pub mod summary_codec;

// The four folders above are the *organisation*; the modules inside them keep their own names, and they are
// re-exported here so that every path written before the reorganisation still resolves — `crate::directive::…`
// inside this crate, `cpp_code_analysis::directive::…` outside it. Moving a file is a change to the filesystem,
// and this is what keeps it from being a change to the API.
//
// * `file`     — one file analysed, and the token of the expanded stream
// * `preprocess` — directives, macros, conditions, guards, expansion (the entry point is this module itself)
// * `include`  — include paths and compiler settings, the file provider, the resolver, the graph
// * `sema`     — names, scopes, the index's declaration facts, and C++20 modules
pub use cache::{
    CACHE_DIRECTORY, FORMAT_VERSION, READING_FINGERPRINT, SummaryKey, content_hash, fnv1a64,
};
pub use file::token;
pub use include::{config, graph, paths, toolchain};
pub use preprocess::{condition, directive, expand, guards, macros};
pub use sema::declarations::{build_facts, by_name, declared_type_of, scope_of};
pub use sema::{module_info, modules, parser_symbols, scopes, symbol};
pub use summary::{
    ConditionAt, ConditionalRegion, DeclFact, DeclKind, FactGuard, FileSummary, GuardBranch,
    IncludeFact, MacroFact, MacroKind, SummaryGuards, macros_from_direct_includes,
    macros_from_direct_includes_with_bodies, macros_from_the_closure_with_bodies, ClosureEvidence,
    macros_in_force_before_the_include, MacroDefinitions,
};
// `guard` is the exception: `preprocess::guard` and `preprocess::guards` differ by one letter, which is exactly
// the hazard the folders are meant to remove, so the *analysis* keeps the plural name and the types are reached
// as `preprocess::guard`.
pub use preprocess::guard;

pub use condition::{
    ConditionExpr, EvalError, Lookup, MacroValues, Value, evaluate, parse_condition,
};
pub use config::{
    CommandLineMacro, CompileCommand, CompileCommands, CompilerConfig, IncludePath,
    parse_compile_commands, predefined_macros_of, split_command_line,
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
    Edge, FileEntry, FileGraph, MAX_INCLUDE_DEPTH, Marked, SkipReason, UnresolvedInclude, Visit,
    WalkScope, file_only, walk, walk_scoped,
};
pub use guard::{Branch, Guard, GuardStack, Region, Visibility};
pub use guards::{FileGuard, GuardAnalysis, analyse_guards, detect_guard};
pub use include::{FoundIn, IncludeResolver, Resolution, Resolved, Unresolved};
pub use toolchain::{
    CommandRunner, DiskCommands, Environment, Output, Toolchain, discover, find_compiler,
    parse_search_list, parse_version, search_paths,
};
// The index layer is flattened like the rest, even though it is the newest: a consumer that wants navigation
// needs `SummaryStore` and `definition_across_files`, and reaching them through two module hops says nothing a
// reader benefits from. `index::summary` is not here — the *shape* of a summary is `summary`, and one type with
// two paths is worse than a longer import.
pub use index::{
    ChangeBatch, EventKind, FileEvent, FileIndexer, FileReferences, IncludeBudget, IncludeIndex,
    IncludeVisibility, MacroReferences, MemberCompletions, MemberList, NameCompletions, NotIndexed,
    NotIndexedReason, OfferedName, PathPattern, Priority, ProjectDefinition, ProjectIndex,
    ProjectMacro, ProjectMember, Reference, ReferenceBudget, ReferenceKind, Rename, Response, Step,
    StepOutcome, StoreStats, SummaryReadError, SummaryStore, UnlistedBase, UnresolvedEdge,
    VisibleDeclaration, WatchFilter, Worklist, definition_across_files, macro_across_files,
    macro_references, member_across_files, member_completions_at, members_of, name_completions_at,
    outcome_of, read_summary, summarize, write_summary,
};
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
pub use scopes::{build_scopes, declared_module_names};
// The driver sits above the folders rather than in one of them: it is the join of all four — the toolchain, the
// include configuration, the summaries and the queries — and putting it inside any one of them would make that
// folder the owner of the others.
pub use session::{FileView, OpenDocuments, Session, SessionFiles};
pub use symbol::{
    Binding, BindingKind, BindingOrigin, DeclName, HeaderName, Known, MaybeName, Name, NameKind,
    QualifiedName, Scope, ScopeId, ScopeKind, ScopeTree, UnknownReason,
};
pub use token::Token;
