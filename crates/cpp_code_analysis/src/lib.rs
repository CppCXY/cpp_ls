//! Code analysis on top of the C++ syntax tree — and on top of `cpp_toolchain`, which is what reads a file the way a
//! compiler does.
//!
//! # What this crate is for
//!
//! `cpp_parser` answers "what does this file *look* like" — a lossless tree of every token, with no opinion about what
//! any of it means. `cpp_toolchain` answers "what does the compiler *read*" — which project this is, which include
//! paths it has, which file includes which, what the macros are at a position, and what one file looks like once they
//! are applied. This crate answers "what does any of it *mean*": scopes, declarations, types, members, overloads, and
//! the diagnostics a reader is shown.
//!
//! # Why the preprocessor is a layer of its own, and why it is a crate
//!
//! The obvious design — run a preprocessor, then parse its output — is the one a compiler uses, and it is unusable for
//! an editor:
//!
//! * **The tree must stay lossless.** Expansion results cannot be written back into the tree without losing the
//!   correspondence between a byte of source and a byte of the file. So expansion produces a *shadow* token stream —
//!   the rendering — and the tree is left alone.
//! * **Both branches of `#if` must stay visible.** A compiler picks one; an editor cannot. Code inside
//!   `#if defined(_WIN32)` is code a person is writing, on whatever platform they happen to be on, and making it
//!   disappear is the failure mode users notice first.
//! * **Every edit would invalidate everything.** Expansion is not local — one `#define` changes the meaning of every
//!   file that includes it — so doing it before parsing would make the parse itself un-incremental.
//!
//! It is a **separate crate** because the direction of the dependency is the design: the preprocessor may not ask this
//! layer what a name means, and while both lived in one crate nothing stopped it. What that buys is the unit the cache
//! is built around — one file's rendering, keyed by its text and its environment — which is a thing the toolchain can
//! produce without a project, an index, or a session.
//!
//! # What this layer answers with
//!
//! ```text
//! a rendering, mapped back      ->  the file's declarations            (index, summary)
//! a name at an offset           ->  a declaration, or why not          (sema::resolve, index::project)
//! a call                        ->  the overload, and what it returns  (index::project::calls)
//! a cursor                      ->  members, parameters, a card        (completion, inlay, hover)
//! a project                     ->  an index, its cache, its session   (index::store, cache, session)
//! ```
//!
//! The vocabulary the preprocessor produced — directives, guards, the shadow stream, the rendering — is re-exported
//! from here (`crate::condition`, `crate::macros`, `crate::preprocess::cooked`, …) so that a consumer of this crate
//! does not have to know which crate below it defined a type it is looking at.

// **Alignment against a real compiler** (`align`) — the one module here whose answer does not come from this crate.
// It puts our token stream beside a real `-E`'s and reports where they differ, by mechanism; see its module
// documentation for why that is the measurement every other module is judged by. It is first in this list because
// everything below it is a claim, and it is the way to check one.
// **Two lints this crate answers with a rationale rather than a change.**
//
// `too_many_arguments`: a question in this layer is asked with `(index, scopes, root, path)` — the engine's context,
// which every walk already carries — and then with what the question is *about*. The four are not a data clump to
// bundle: a caller borrows them from the session it holds, so a struct would be rebuilt per call to re-borrow the
// same four fields. The largest of these functions is `walk_one_file` at thirteen, and it is a recursion over the
// preprocessor's state, not a public API.
//
// `type_complexity`: the memo tables are spelled out where they are declared —
// `RefCell<HashMap<(u32, u32), Known<(Type, PathBuf)>>>` is the answer to "what have I already been asked", and naming
// it `TypeMemo` would move the shape away from the one place a reader checks it against the code that inserts into it.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]
#[macro_use]
extern crate rust_i18n;

// **The diagnostics speak the reader's language.** The keys are the English templates themselves
// (`t!("`{}` is declared here and never used")`), so `en` needs no file — a missing key answers with the key, which
// *is* the English message — and `locales/zh-CN.yml` carries the translations. `cpp_parser` loads its own table the
// same way, and one call to `cpp_parser::set_locale` switches both: rust-i18n's current locale is process-wide.
rust_i18n::i18n!("./locales", fallback = "en");

pub mod cache;
pub mod file;
pub mod index;
pub mod sema;
pub mod session;

// **The toolchain and the preprocessor live in `cpp_toolchain`** — the layer below this one, which finds a
// translation unit, configures it, walks it and renders one file of it. The re-exports are deliberate and are the
// whole of the migration's API promise: every path written before the split still resolves — `crate::include::…`,
// `crate::macros::…`, `crate::probes::…` — so the split is a change to the filesystem, not to the vocabulary.
//
// The direction is one-way by construction, and that is the point of the crate: the preprocessor may not ask the
// index what a name means.
pub use cpp_toolchain::{include, preprocess, project};

// **The layers hold the files; the root holds the names.** Everything that used to be a top-level module is a
// layer's now — a cached reading is `cache::…`, the effect a file has on the macro table is `index::…`, a feature a
// cursor asks for is `session::…` — and these re-exports keep every path that was written before the move resolving.
// One of them is said from outside this crate: `inspect` reaches `TypeHintKind` through the root.
pub use cache::{align, env, memory, summary, summary_codec, unit_codec};
pub use index::{effect, surface};
pub use session::{completion, cuda, definition, doc, folding, format, inlay, semantic, signature};

// The folders are the *organisation*; the modules inside them keep their own names, and they are re-exported here so
// that every path written before a reorganisation still resolves. The four that moved down a layer keep their
// re-export as well — `include`, `preprocess`, `project`, `probes` — so a consumer that already says
// `cpp_code_analysis::include::config::CompilerConfig` needs no change; a new one should reach for `cpp_toolchain::…`
// directly and skip the hop.
//
// The five layers this crate is made of, bottom up:
//
// * `sema`     — names, scopes, types, overloads: what the parsed text *means*
// * `file`     — one file analysed: the VFS holding texts, a view parsed, and the coordinates between them
// * `cache`    — the on-disk store and **what it stores**: a summary, its two codecs, and the cost report
// * `index`    — the summaries in memory, the store, and every query over them (plus a file's macro `effect`)
// * `session`  — a project opened: the pump, the caches, the reads, and **every request a client can make**
//
// A request module under `session` is a *feature*: `completion`, `inlay`, `signature`, `semantic`, `folding`,
// `definition`, `doc`, and the format-string reader the hover uses. What such a module shares with the layers below
// does not live in it — reading a call's arguments is `sema::call_syntax` — because a feature the index depends on is
// a dependency pointing the wrong way.
pub use cache::{
    CACHE_DIRECTORY, FORMAT_VERSION, READING_FINGERPRINT, SummaryKey, content_hash, fnv1a64,
};
// The alignment layer, flattened like the rest: a consumer that wants to check a reading against a compiler needs
// `Report::of` and `tokenize_preprocessed`, and reaching them through the module hop says nothing a reader benefits
// from. `MAX_EDIT_DISTANCE` is here because a caller has to be able to say how far a comparison was allowed to go
// before it reports a `truncated` one.
pub use align::{
    Alignment, Difference, DiskTexts, MAX_EDIT_DISTANCE, Reason, Report, Side, StreamToken,
    UnitTexts, align, files_of, normalized, shared, tokenize_preprocessed, unit_tokens,
    without_pragmas,
};
// The *completion* layer is the one place where the answer to a cursor is assembled rather than queried: the two
// name queries below it answer "what is visible" and "what does this type have", and this decides which of those
// questions the cursor is asking and in what order the answer is shown. See `completion`'s module documentation.
pub use completion::{
    BUILTIN_TYPES, CompletionContext, CompletionItem, CompletionSet, DIRECTIVES, DirectiveName,
    Header, HeaderIndex, HeaderKind, ItemKind, KEYWORDS, Keyword, KeywordUse, SNIPPETS,
    SharedHeaders, Snippet, completion_at, context_at, detail_of, header_index, why_no_members,
};
pub use cooked::{FileMacros, Over, ParsedDefinitions, configuration_from_environment_and};
pub use file::token;
pub use include::{config, graph, msvc, paths, system_headers, toolchain};
pub use macros::{MacroBindings, UnitSources};
pub use preprocess::{condition, cooked, directive, expand, guards, macros};
pub use sema::check::{CheckConfig, CheckContext, DiagnosticCode, Finding};
pub use sema::declarations::{build_facts, by_name, declared_type_of, scope_of};
pub use sema::{check, module_info, modules, parser_symbols, scopes, symbol};
// `tu_cache` is gone: a built `TranslationUnit` is not persisted any more — see `Session::translation_unit_of`.

pub use memory::MemoryReport;
pub use summary::ConditionalBody;
pub use summary::{
    ClosureEvidence, ConditionAt, ConditionalRegion, CookedDiagnostic, DeclFact, DeclKind,
    DeclModifiers, FactGuard, FileSummary, GuardBranch, IncludeFact, IndexedRendering, IndexedUnit,
    MacroAt, MacroDefinitions, MacroFact, MacroKind, MacroView, MapReport, OutlineSymbol,
    ReflectionFact, RenderedUnit, SummaryGuards, TranslationUnit, UnitDefinitions, UnitReading,
    UnitSpan, macros_from_direct_includes, macros_from_direct_includes_with_bodies,
    macros_from_the_closure_with_bodies, macros_in_force_before_the_include, outline_of,
};
// `guard` is the exception: `preprocess::guard` and `preprocess::guards` differ by one letter, which is exactly
// the hazard the folders are meant to remove, so the *analysis* keeps the plural name and the types are reached
// as `preprocess::guard`.
pub use preprocess::guard;

pub use condition::{
    ConditionExpr, EvalError, Lookup, MacroValues, NoMacros, Value, evaluate, parse_condition,
};
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
pub use file::{FileAnalysis, FileTokens};
pub use graph::{
    Edge, FileEntry, FileGraph, MAX_INCLUDE_DEPTH, Marked, SkipReason, UnresolvedInclude, Visit,
    WalkScope, file_only, walk, walk_scoped,
};
pub use guard::{Branch, Guard, GuardStack, Region, Visibility};
pub use guards::{FileGuard, GuardAnalysis, analyse_guards, detect_guard};
pub use include::toolchain::BuildStatement;
pub use include::{FoundIn, IncludeResolver, Resolution, Resolved, Unresolved};
pub use toolchain::{
    CommandRunner, DiskCommands, Environment, Output, Toolchain, ToolchainSource, discover,
    discover_with, find_compiler, parse_search_list, parse_version, search_paths,
};
// The index layer is flattened like the rest, even though it is the newest: a consumer that wants navigation
// needs `SummaryStore` and `definition_across_files`, and reaching them through two module hops says nothing a
// reader benefits from. `index::summary` is not here — the *shape* of a summary is `summary`, and one type with
// two paths is worse than a longer import.
pub use cooked::{
    Configuration, CookedStream, RenderedCooked, RenderedSpan, configuration_from_environment,
    configuration_from_environment_with, cook, cook_with,
};
pub use index::walk_cache::WalkCache;
pub use index::{
    ChangeBatch, CookedFile, EventKind, FileEvent, FileIndexer, FileReferences, HeaderTarget,
    IncludeBudget, IncludeIndex, IncludeVisibility, MacroReferences, MemberCompletions, MemberList,
    NameCompletions, NameProvenance, NotIndexed, NotIndexedReason, OfferedName, PathPattern,
    Priority, ProjectDefinition, ProjectDefinitions, ProjectIndex, ProjectMacro, ProjectMember,
    ProjectSymbol, Reference, ReferenceBudget, ReferenceKind, Rename, Response, Step, StepOutcome,
    StoreStats, SummaryReadError, SummaryStore, SymbolReferences, SymbolToFind, UnlistedBase,
    UnresolvedEdge, VisibleDeclaration, WatchFilter, Worklist, definition_across_files, header_at,
    macro_across_files, macro_references, member_across_files, member_completions_at, members_of,
    name_completions_at, outcome_of, summarize, symbol_references,
};
pub use macros::{MacroBody, MacroDef, MacroTable, Parameter, ParameterKind};
pub use module_info::{ImportDeclaration, ImportTarget, ModuleInfo, ModuleUnit};
pub use modules::{
    ImportEdge, ImportOutcome, MAX_IMPORT_DEPTH, ModuleGraph, ModuleScanner, ModuleUnitEntry,
    scan_imports,
};
pub use paths::{
    CachedFiles, DiskFiles, FileId, FileProvider, MemoryFiles, OverlayFiles, PathInterner,
    join_normalized, normalize_path, parent_normalized,
};
pub use preprocess::{FilePreprocessing, PositionalMacros, preprocess};
pub use project::{
    CONFIGURATION_FILE_NAMES, CompileSection, ConfigProblem, ConfigReport, DEFAULT_FORMATTER,
    Database, DatabaseOrigin, DiagnosticsSection, DiscoveryProblem, FormatStringsSection,
    FormatSyntax, FormattingSection, FunctionEntry, HoverSection, IndexSection,
    PROJECT_CONFIG_FILE, ProjectConfig, ProjectDiscovery, Severity, WorkspaceSection,
    find_database, load_config, parse_config,
};
pub use summary::ModuleReading;
// `MacroBodies` is the parser's trait, re-exported here because this crate's public API names it: the seam between
// "a file's tokens" and "what its includes say the macros mean" is one trait, and a consumer should not have to
// know which crate defined it.
pub use cpp_parser::MacroBodies;
pub use scopes::{NoMacroBodies, build_scopes, declared_module_names};
// The driver sits above the folders rather than in one of them: it is the join of all four — the toolchain, the
// include configuration, the summaries and the queries — and putting it inside any one of them would make that
// folder the owner of the others.
pub use inlay::{ParameterHint, TypeHint, TypeHintKind};
pub use session::{
    CUDA_SOURCE_EXTENSIONS, CookedReading, DiagnosticReading, ExpressionType, FileDiagnostic,
    FileDiagnostics, ModuleImport, ModuleNote, ModuleUnitTarget, OpenDocuments, Session,
    SessionFiles, UnitStats, WrittenAttribute, cuda_source_extensions, source_extensions,
};
// The *file* layer's two public faces: a view is one file parsed (what a cursor query is answered against), and a
// VFS is the set of files being held (their text, and the line index of that text). They live in `file` rather
// than next to the session because they are about files, not about a project.
pub use file::vfs::{Vfs, VfsFile};
pub use file::view::FileView;
pub use symbol::{
    Binding, BindingKind, BindingOrigin, DeclName, HeaderName, Known, MaybeName, Name, NameKind,
    QualifiedName, Scope, ScopeId, ScopeKind, ScopeTree, UnknownReason,
};
pub use token::Token;
