//! Building one file's [`FileSummary`] — the join nothing else in this crate performs.
//!
//! # Why this needs a module of its own
//!
//! The three passes a summary is made of are each somebody else's job and none of them knows about the others:
//!
//! ```text
//! cpp_parser                 the tree
//! preprocess::preprocess     the directives, the macro table, the conditions
//! sema::build_scopes         the scopes and their bindings
//! declarations::build_facts  the declarations *and* which `#if` each was written in   ← needs all three
//! ```
//!
//! [`crate::build_facts`] needs the scopes *and* the preprocessing, because a declaration has to say which
//! conditional region it is in — and neither of those layers can produce it alone. That join is what this module
//! is, and writing it anywhere else would mean one of the three layers reaching into another.
//!
//! # What is deliberately not here
//!
//! No caching, no filesystem walks, no invalidation. This builds a summary from text it is given; deciding
//! *whether* to build one, and what to do with the result, is [`crate::cache`]'s and the consumer's business.
//! Keeping the decision out is what makes this testable on a string with no project around it.
//!
//! # Why the resolver is optional
//!
//! An include's target is a fact about the file, but *where it resolved to* is a fact about the project and its
//! include paths. A caller with no configuration — a test, or an editor looking at a file outside any project —
//! still gets every other fact, and the includes come out unresolved with their spelling intact. That is the
//! honest answer rather than a missing one: the file said `#include "widget.h"` and nothing here knows where
//! that is.

use std::path::{Path, PathBuf};

use cpp_parser::{CppParser, CppSyntaxTree, ParserConfig};

pub mod environment;
pub mod project;
pub mod references;
pub mod store;
pub mod watch;
pub mod worklist;

pub use environment::{MacrosHere, visibility_at};

pub use project::{
    IncludeVisibility, MemberCompletions, MemberList, NameCompletions, OfferedName, ProjectDefinition,
    ProjectIndex, ProjectMacro, ProjectMember, UnlistedBase, VisibleDeclaration,
    definition_across_files, macro_across_files, member_across_files, member_completions_at, members_of,
    name_completions_at,
};
pub use references::{
    FileReferences, MacroReferences, Reference, ReferenceBudget, ReferenceKind, Rename,
    macro_references,
};
pub use store::{
    IncludeBudget, IncludeIndex, NotIndexed, NotIndexedReason, StoreStats, SummaryStore,
    UnresolvedEdge,
};
pub use watch::{ChangeBatch, EventKind, FileEvent, Response, WatchFilter};
pub use worklist::{Priority, Step, StepOutcome, Worklist, outcome_of};

use crate::cache::{SummaryKey, content_hash};
use crate::include::config::CompilerConfig;
use crate::include::paths::{FileProvider, PathInterner};
use crate::include::IncludeResolver;
use crate::preprocess::directive::{Directive, SpannedDirective};
use crate::preprocess::preprocess;
use crate::sema::declarations::{assign_guards, build_facts, mark_settling_macro_facts};
use crate::sema::scopes::build_scopes;
use crate::summary::{FactGuard, FileSummary, IncludeFact, MacroFact};
use crate::summary_codec::DecodeError;

/// Everything needed to turn a file's text into a summary, minus the text.
///
/// The resolver is a [`FileProvider`] and a [`CompilerConfig`] rather than an [`IncludeResolver`] because the
/// resolver borrows both, and a caller indexing a project wants to keep the configuration and the provider around
/// across calls.
pub struct FileIndexer<'a, F: FileProvider> {
    files: &'a F,
    config: &'a CompilerConfig,
}

impl<'a, F: FileProvider> FileIndexer<'a, F> {
    pub fn new(files: &'a F, config: &'a CompilerConfig) -> Self {
        FileIndexer { files, config }
    }

    /// Build the summary of the file at `path`, whose text is `source`.
    ///
    /// `key` is the caller's, because only the caller knows the compilation context — the configuration and the
    /// directory the file sits in. It is stored in the summary rather than recomputed, so that a loaded entry can
    /// be checked against the file it claims to describe.
    ///
    /// # The one part of the key that is *not* the caller's
    ///
    /// `key.content_hash` is **replaced** by the hash of `source`. `source` is the same text that was just parsed,
    /// and no caller can describe it more accurately than hashing it — while a caller that got it wrong would
    /// store a summary under a name that does not describe its own text, which is a wrong answer rather than a
    /// cache miss. The authority is here because the text is here.
    pub fn index(&self, path: &Path, source: &str, key: SummaryKey) -> FileSummary {
        // Parsed **for the configuration's target**: which compiler's reserved spellings mean what is part of the
        // compilation, not of the text — `__int128` is a type to g++ and a name to cl.exe. See
        // [`CompilerConfig::dialect`], and note that the same value is part of `context_hash`, so a summary
        // written for one target is never read as if it were written for the other.
        let tree = CppParser::parse(
            source,
            ParserConfig::default().with_dialect(self.config.dialect()),
        );
        self.index_tree(path, source, &tree, key)
    }

    /// [`FileIndexer::index`] for a caller that already has the tree.
    ///
    /// Parsing twice is the most expensive thing this layer can be asked to do, and an editor usually has the tree
    /// already — it is what it is displaying.
    pub fn index_tree(
        &self,
        path: &Path,
        source: &str,
        tree: &CppSyntaxTree,
        key: SummaryKey,
    ) -> FileSummary {
        // See `index`: the content part of the key is the text, always. The caller supplies the part that
        // describes the *compilation* — the configuration and the directory the file sits in.
        let key = SummaryKey::new(content_hash(source), key.context_hash);

        let root = tree.get_red_root();
        let preprocessing = preprocess(&root);
        let scopes = build_scopes(&root);

        // The diagnostics, as ranges, for the one field a fact takes from them rather than from the tree — see
        // [`DeclFact::clean`]. Collected once for the whole file: the parser reports a handful per file, and
        // asking per declaration would be a scan of the list per fact.
        let errors: Vec<cpp_parser::SourceRange> = tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();

        let (mut declarations, mut guards) =
            build_facts(&scopes, &preprocessing, &root, &errors);

        let mut macros: Vec<MacroFact> = preprocessing
            .directives
            .iter()
            .filter_map(macro_fact)
            .collect();

        // The interner is local: resolving an include mints an id as a side effect, and the id is discarded
        // because a summary stores the *path*. A `FileId` is an index into a run's interner and means nothing to
        // whoever reads the summary back.
        let mut interner = PathInterner::new(cfg!(windows));
        let resolver = IncludeResolver::new(self.files, self.config);
        let directory = path.parent().unwrap_or(Path::new("."));

        let mut includes: Vec<IncludeFact> = preprocessing
            .directives
            .iter()
            .filter_map(|spanned| {
                include_fact(
                    &spanned.directive,
                    spanned.range,
                    directory,
                    &resolver,
                    &mut interner,
                )
            })
            .collect();

        // Every kind of fact carries a guard, and they are assigned together rather than per kind: the sweep is
        // over *offsets*, and running it once per fact list would rebuild the region list each time. An include's
        // guard is the one that earns its keep — it is what a cross-file lookup reads to decide whether a
        // declaration reached through that include is unconditionally in scope — but a `#define` inside an `#if`
        // is the same question about a macro.
        macros.sort_by_key(|fact| fact.range.start_offset);
        includes.sort_by_key(|fact| fact.range.start_offset);

        let mut guarded: Vec<(&mut FactGuard, usize)> = macros
            .iter_mut()
            .map(|fact| (&mut fact.guard, fact.range.start_offset))
            .chain(
                includes
                    .iter_mut()
                    .map(|fact| (&mut fact.guard, fact.range.start_offset)),
            )
            .collect();
        guarded.sort_by_key(|(_, at)| *at);

        assign_guards(&mut guarded, &preprocessing, &mut guards);

        // The file's **own include guard is not a condition**, and this is the step that makes the standard
        // library queryable at all: a header puts its whole body inside `#ifndef _GLIBCXX_STRING`, so without this
        // rule every `#include` written inside a header is "conditional" and every declaration reached through one
        // is `ConditionalCompilation` — measured on the closure of `<string>`, that is *every* cross-file answer
        // there is. See `deguard_the_files_own_guard` for why calling it unconditional is the honest reading.
        let own_guard = own_guard_region(&preprocessing, &root);

        // Stored, not just used here: a *walk* evaluating a condition needs the same rule, and it has only the
        // summary. See `SummaryGuards::own_guard` — a file's own guard is not a condition on anything.
        guards.own_guard = own_guard.map(|region| region as u32);

        if let Some(region) = own_guard {
            let mut all: Vec<&mut FactGuard> = declarations
                .iter_mut()
                .map(|fact| &mut fact.guard)
                .chain(macros.iter_mut().map(|fact| &mut fact.guard))
                .chain(includes.iter_mut().map(|fact| &mut fact.guard))
                .collect();
            deguard_the_files_own_guard(&mut all, region);
        }

        // What the guards above cannot say on their own: whether a `#define` inside an `#if` still *settles* the
        // name whichever branch is taken — the `#ifndef NAME / #define NAME` idiom, which is how the system headers
        // define most of the macros a project uses. It runs after the de-guard step because a fact already
        // `Unconditional` has nothing to settle, and it is told which region the own guard is because the rule it
        // applies nests inside conditionals and must agree with that step about what a file guard means.
        mark_settling_macro_facts(&preprocessing, own_guard, &mut macros);

        FileSummary {
            path: path.to_path_buf(),
            key,
            declarations,
            macros,
            includes,
            guards,
        }
    }
}

/// A summary of a file with **no project around it**: every include unresolved, every macro recorded.
///
/// The entry point for a caller that has text and nothing else — a test, or an editor on a file outside any
/// project. Equivalent to a [`FileIndexer`] whose resolver finds nothing, and named separately so that the
/// ordinary case does not have to construct a configuration it is not going to use.
pub fn summarize(path: &Path, source: &str, key: SummaryKey) -> FileSummary {
    /// A provider that has no files, so every include comes out unresolved.
    struct NoFiles;

    impl FileProvider for NoFiles {
        fn read(&self, _path: &Path) -> Option<String> {
            None
        }

        fn exists(&self, _path: &Path) -> bool {
            false
        }
    }

    FileIndexer::new(&NoFiles, &CompilerConfig::default()).index(path, source, key)
}

/// The [`MacroFact`] for a `#define` or an `#undef`, or `None` for every other directive.
///
/// Both are facts about the same name's history, and a query needs both to be answerable: a name `#undef`ed above
/// the cursor is not a macro, and a table that only remembers definitions would point at a `#define` that is no
/// longer in force. See [`crate::MacroKind`].
///
/// `spanned` rather than the bare directive because an `#undef`'s *name* position is not tracked by the directive
/// reader — it keeps the name and no range — so the fact carries the directive's range, which is the line to show
/// a user asking where a macro stops being one.
fn macro_fact(spanned: &SpannedDirective) -> Option<MacroFact> {
    match &spanned.directive {
        Directive::Define(define) => {
            let definition = define.macro_def.as_ref()?;

            Some(MacroFact {
                name: definition.name.to_string(),
                kind: crate::summary::MacroKind::Definition,
                function_like: definition.is_function_like(),
                body: macro_body_shape(definition),
                value: macro_value(definition),
                // The *name's* range, not the directive's: "go to macro definition" is a jump to the name a user
                // can see, and a rename edits it. The guard sweep reads this fact's offset too, and the name is
                // inside the same conditional region as the directive that wrote it — a `#define` cannot span an
                // `#endif`.
                range: definition.name_range,
                guard: crate::summary::FactGuard::Unconditional,
                // Filled in by `mark_settling_macro_facts`, which is the only place that knows where the
                // conditionals are; see `MacroFact::settles_the_name`.
                settles_the_name: false,
            })
        }
        Directive::Undef { name: Some(name) } => Some(MacroFact {
            name: name.to_string(),
            kind: crate::summary::MacroKind::Undefinition,
            function_like: false,
            // Nothing to say about a body, and saying `Unknown` is the honest way to say it.
            body: cpp_parser::MacroBody::Unknown,
            // An `#undef` has no value: the name stops being a macro, which is the whole of what it says.
            value: None,
            range: spanned.range,
            guard: crate::summary::FactGuard::Unconditional,
            settles_the_name: false,
        }),
        _ => None,
    }
}

/// The value a condition could read out of a definition, or `None`.
///
/// One integer literal in the body, and nothing else — which is exactly what
/// [`crate::condition::macro_value`] accepts, so the two agree by construction rather than by convention. A
/// function-like macro is not a value at all (`#if F(x)` is not how a macro is asked about), and neither is a body
/// of two tokens, a body that is a name, or an empty body: a condition asking about any of them is `Unknown`, and
/// storing a spelling that no evaluator can use would only make the cache bigger.
fn macro_value(definition: &crate::preprocess::macros::MacroDef) -> Option<Box<str>> {
    if definition.is_function_like() {
        return None;
    }

    let mut significant = definition.body.significant();
    let only = significant.next()?;

    if significant.next().is_some() {
        return None;
    }

    (only.kind == cpp_parser::CppTokenKind::IntegerLiteral).then(|| only.text.clone())
}

/// The region a file's own include guard opens, when it has one.
///
/// `Some(0)` or `None`, and the zero is not a coincidence: [`crate::detect_guard`] only recognises a guard that is
/// the file's **first** conditional, at depth 0, with no declaration before it — so when there is a guard, the
/// first region in the list is the one it opens. `#pragma once` is a guard with no region at all, which is why
/// the answer is an index rather than a boolean.
fn own_guard_region(
    preprocessing: &crate::FilePreprocessing,
    root: &cpp_parser::CppSyntaxNode,
) -> Option<usize> {
    matches!(
        crate::guards::detect_guard(&preprocessing.directives, root),
        crate::guards::Guard::Macro(_)
    )
    .then_some(0)
}

/// Treat everything inside a file's own include guard as **unconditional**.
///
/// # Why a guard is not a condition
///
/// A `#if` makes a fact conditional because whether the branch was taken depends on macros this analysis does not
/// have — the fact might be invisible, so a consumer is told `Unknown` rather than `Yes`. A file's **own** guard is
/// the one `#if` where that reasoning does not hold: the condition is `#ifndef _GLIBCXX_STRING`, and *entering the
/// file at all* is what defines `_GLIBCXX_STRING`. Every inclusion of a guarded header that does anything takes
/// the branch; the visits that do not are the ones where the header has already been read, and the declarations
/// are visible from those too.
///
/// So a declaration inside the guard is visible to **any** file that includes the header, which is exactly what
/// `Unconditional` means here, and what it does not mean is "there was no `#if` in the text" — the region is
/// still in [`crate::SummaryGuards::regions`], and the guard macro is still a fact of the file.
///
/// # What it costs, measured
///
/// Without it, the closure of `<string>` answers **every** cross-file query with `ConditionalCompilation`: the
/// standard library's headers guard their bodies, so every `#include` inside one is inside a region.
/// `examples/std_query.rs` is the probe that shows it — 0 of 7 ordinary queries resolved before this rule.
fn deguard_the_files_own_guard(facts: &mut [&mut FactGuard], region: usize) {
    for fact in facts {
        if **fact == FactGuard::Region(region as u32) {
            **fact = FactGuard::Unconditional;
        }
    }
}

/// The shape of a macro's body, in the vocabulary the parser's rules ask about.
///
/// This classifier is the join between the two ends of a macro's life. On one side
/// [`crate::preprocess::macros::MacroBody`] holds the body's **tokens**, because expansion needs them; on the
/// other [`cpp_parser::MacroBody`] is the *shape* the grammar acts on — `Statement` for a macro whose invocation
/// needs no `;`, `Specifier` for one that stands where a declaration specifier goes. Nothing else converts
/// between them, so every macro-shaped reading in the parser ends up guessing; this is the evidence that replaces
/// the guess.
///
/// # How the shape is decided
///
/// By the first and last significant tokens, which is what a reader uses too:
///
/// ```text
/// __declspec(dllexport)      a specifier — call-like, and it is not a statement
/// do { … } while (false)     a statement that brings its own `;`-free block
/// if (x) { … }               a statement
/// ((a) > (b) ? (a) : (b))    an expression
/// int                        a type
/// { … }                      a block, always followed by `{`
/// ```
///
/// The order of the tests is the order the shapes exclude each other: a body that opens with a control keyword or
/// `do` is a statement whatever else is in it, and only a body that is *entirely* a parenthesised expression is an
/// expression — `(a) + (b)` is a sum, and calling a sum an expression is right, while calling `((a) > (b) ? …)` a
/// statement would be wrong.
///
/// `MacroBody::Unknown` is the answer when nothing matches, and it is not a failure: it already rules out the
/// *call* reading, which is most of what a caller gets from knowing a name is a macro.
fn macro_body_shape(definition: &crate::preprocess::macros::MacroDef) -> cpp_parser::MacroBody {
    use cpp_parser::MacroBody;
    use cpp_parser::CppTokenKind;

    let tokens: Vec<CppTokenKind> = definition
        .body
        .significant()
        .map(|token| token.kind)
        .collect();

    let Some(first) = tokens.first().copied() else {
        // `#define FOO` — a macro that expands to nothing. Used for feature flags, so it is ordinary rather than
        // malformed; with no body there is no shape to report.
        return MacroBody::Unknown;
    };

    // A body that is only a brace is the gtest shape: the invocation is always followed by the block it wrote.
    if first == CppTokenKind::LeftBrace && tokens.last().copied() == Some(CppTokenKind::RightBrace) {
        return MacroBody::Block;
    }

    if matches!(
        first,
        CppTokenKind::IfKeyword
            | CppTokenKind::ForKeyword
            | CppTokenKind::WhileKeyword
            | CppTokenKind::SwitchKeyword
            | CppTokenKind::DoKeyword
            | CppTokenKind::ReturnKeyword
            | CppTokenKind::ThrowKeyword
            | CppTokenKind::TryKeyword
            | CppTokenKind::BreakKeyword
            | CppTokenKind::ContinueKeyword
            | CppTokenKind::GotoKeyword
    ) {
        return MacroBody::Statement;
    }

    // A specifier: an attribute, an export marker, a calling convention. Two spellings reach here and they need
    // separate tests, because one is C++ and the other is not:
    //
    // * the specifier *keywords* — `static`, `extern`, `inline` — which no other construct can begin with;
    // * the compiler's own attribute spelling — `__declspec(dllexport)`, `__attribute__((…))` — which is an
    //   identifier followed by a parenthesised group, and therefore shaped exactly like `MAX(a, b)`.
    if is_specifier_like(tokens[0]) || is_compiler_specifier_macro(&tokens, first_spelling(definition)) {
        return MacroBody::Specifier;
    }

    // A type: the body is one or two type keywords and nothing else — `#define MY_INT int`. A second *word* rules
    // it out, because `int x` is a declaration and not a type.
    if tokens.iter().all(|kind| is_type_word(*kind)) {
        return MacroBody::Type;
    }

    // An expression: everything in the body is an operand, an operator, or a bracket, and the whole body is
    // parenthesised. The parenthesis test is what separates `((a) > (b) ? (a) : (b))` from `(a) + (b)`, which is
    // also an expression — and both answers are `Expression`, so the test is about *confidence*: a body that is
    // one parenthesised group is an expression whatever is inside it.
    if is_one_parenthesised_group(&tokens) {
        return MacroBody::Expression;
    }

    MacroBody::Unknown
}

/// Is this token only ever a declaration specifier?
fn is_specifier_like(kind: cpp_parser::CppTokenKind) -> bool {
    use cpp_parser::CppTokenKind as K;

    matches!(
        kind,
        K::ExternKeyword
            | K::StaticKeyword
            | K::InlineKeyword
            | K::ConstexprKeyword
            | K::VirtualKeyword
            | K::ExplicitKeyword
            | K::MutableKeyword
            | K::ThreadLocalKeyword
            | K::TypedefKeyword
            | K::FriendKeyword
    )
}

/// Is this token a word that can name a type on its own?
fn is_type_word(kind: cpp_parser::CppTokenKind) -> bool {
    use cpp_parser::CppTokenKind as K;

    matches!(
        kind,
        K::VoidKeyword
            | K::BoolLiteral
            | K::CharKeyword
            | K::ShortKeyword
            | K::IntKeyword
            | K::LongKeyword
            | K::FloatKeyword
            | K::DoubleKeyword
            | K::SignedKeyword
            | K::UnsignedKeyword
    )
}

/// Is this macro body a compiler-specific **specifier** spelling?
///
/// `__declspec(dllexport)` and `__attribute__((visibility("default")))` are what an export macro is written as on
/// the two mainstream compilers, and both are *call-shaped*: an identifier followed by a parenthesised group. They
/// are not C++ at all, so no rule of the grammar could recognise them — but a macro whose body is one of them
/// stands exactly where a declaration specifier does, and that is the fact the parser needs from this classifier.
///
/// Recognised **by name** rather than by shape alone, because the shape is shared with every other function-like
/// macro: `MAX(a, b)` is also an identifier and a parenthesised group, and calling that a specifier would make
/// `MAX(1, 2)` a declaration. The names are the compilers', so the list does not grow with user code.
fn is_compiler_specifier_macro(tokens: &[cpp_parser::CppTokenKind], spelling: &str) -> bool {
    use cpp_parser::CppTokenKind as K;

    if tokens.first().copied() != Some(K::Identifier)
        || tokens.get(1).copied() != Some(K::LeftParen)
        || tokens.last().copied() != Some(K::RightParen)
    {
        return false;
    }

    matches!(
        spelling,
        "__declspec" | "__attribute__" | "__attribute" | "__cdecl" | "__stdcall" | "__fastcall"
    )
}

/// The spelling of a macro body's first significant token, for the tests that have to read it by name.
fn first_spelling(definition: &crate::preprocess::macros::MacroDef) -> &str {
    definition
        .body
        .significant()
        .next()
        .map(|token| token.text.as_ref())
        .unwrap_or("")
}

/// Is the whole body wrapped in one pair of parentheses?
fn is_one_parenthesised_group(tokens: &[cpp_parser::CppTokenKind]) -> bool {
    use cpp_parser::CppTokenKind as K;

    if tokens.first().copied() != Some(K::LeftParen) || tokens.last().copied() != Some(K::RightParen) {
        return false;
    }

    // The group has to *close* at the last token, not earlier: `(a) + (b)` also starts with `(` and ends with
    // `)`, and it is not one group.
    let mut depth = 0isize;
    for (index, kind) in tokens.iter().enumerate() {
        match kind {
            K::LeftParen => depth += 1,
            K::RightParen => {
                depth -= 1;
                if depth == 0 {
                    return index == tokens.len() - 1;
                }
            }
            _ => {}
        }
    }

    false
}

/// The [`IncludeFact`] for an `#include`, or `None` for every other directive.
///
/// A resolution failure is not an error here: an include that was not found is stored with its spelling and no
/// path, which is what a consumer needs to report "cannot find `widget.h`" rather than to hide it.
fn include_fact<F: FileProvider>(
    directive: &Directive,
    range: cpp_parser::SourceRange,
    including: &Path,
    resolver: &IncludeResolver<'_, F>,
    interner: &mut PathInterner,
) -> Option<IncludeFact> {
    let Directive::Include(include) = directive else {
        return None;
    };

    let resolved: Option<PathBuf> = resolver
        .resolve(include, including, None, interner)
        .resolved()
        .map(|resolved| resolved.path.clone());

    Some(IncludeFact {
        form: include.form,
        spelling: include.target.to_string(),
        resolved,
        is_next: include.is_next,
        range,
        guard: crate::summary::FactGuard::Unconditional,
    })
}

/// Put a summary on disk, under the path its key names.
///
/// The write is a `.tmp` file followed by a rename, so a crash mid-write leaves the old summary rather than half
/// of a new one — and a half-written summary is worse than none, because a *wrong* entry answers queries that a
/// missing one would have sent to a rebuild.
pub fn write_summary(summary: &FileSummary, project_root: &Path) -> std::io::Result<PathBuf> {
    let path = summary.key.path_under(project_root);
    let bytes = crate::summary_codec::encode(summary);

    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }

    let temporary = path.with_extension("bin.tmp");
    std::fs::write(&temporary, &bytes)?;
    std::fs::rename(&temporary, &path)?;

    Ok(path)
}

/// Read a summary from disk, or say why it could not be used.
///
/// Every failure is the caller's cue to rebuild: a missing file, a corrupt one, and one written by a different
/// format version are the same decision, and the distinctions are kept because only the last two are worth
/// logging.
pub fn read_summary(path: &Path) -> Result<FileSummary, SummaryReadError> {
    let bytes = std::fs::read(path).map_err(SummaryReadError::Io)?;
    crate::summary_codec::decode(&bytes).map_err(SummaryReadError::Decode)
}

/// Why a stored summary could not be read.
#[derive(Debug)]
pub enum SummaryReadError {
    /// The file could not be read at all.
    Io(std::io::Error),
    /// The bytes are not a summary this build can use.
    Decode(DecodeError),
}

impl std::fmt::Display for SummaryReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SummaryReadError::Io(error) => write!(f, "cannot read the summary: {error}"),
            SummaryReadError::Decode(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SummaryReadError {}

#[cfg(test)]
mod tests {
    use super::{FileIndexer, include_fact, macro_body_shape, summarize, write_summary};
    use crate::cache::SummaryKey;
    use crate::include::config::CompilerConfig;
    use crate::include::paths::{FileProvider, MemoryFiles, PathInterner};
    use crate::include::IncludeResolver;
    use crate::preprocess::directive::{Directive, IncludeForm};
    use crate::preprocess::macros::MacroTable;
    use crate::preprocess::preprocess;
    use crate::summary::{DeclKind, FactGuard, MacroKind};
    use cpp_parser::{CppParser, MacroBody, ParserConfig};
    use std::path::{Path, PathBuf};

    fn key() -> SummaryKey {
        SummaryKey::new(0, 0)
    }

    fn summary(source: &str) -> crate::summary::FileSummary {
        summarize(Path::new("/p/widget.cpp"), source, key())
    }

    #[test]
    fn a_summary_is_read_for_the_compiler_it_was_configured_with() {
        // The dialect has to reach the **parser**, not just the key: `int __int128;` declares a variable called
        // `__int128` when the compiler spells `__int128` as an ordinary name (MSVC), and declares **nothing** when
        // it is a type keyword (GNU) — a name cannot be a declarator if it is a type. A configuration that stopped
        // at the cache key would hash two targets apart and then read both the same way, which is the worst of
        // both.
        //
        // The spelling this test used before was `unsigned __int128 x;`, and it stopped distinguishing the two
        // dialects when the specifier sequence learned to let a name join a type that is already there
        // (`docs/grammar-gaps.md` B72): `__int128 x` is a type and a declarator under *both* dialects now, which
        // is the better reading of both. What the dialect decides is whether the token **can** be a declarator, and
        // that is what this spelling asks.
        let files = MemoryFiles::new();
        let names = |dialect: cpp_parser::Dialect| {
            let config = CompilerConfig::default().with_dialect(dialect);
            let summary = FileIndexer::new(&files, &config).index(
                Path::new("/p/a.cpp"),
                "int __int128;\n",
                key(),
            );
            summary
                .declarations
                .iter()
                .map(|fact| fact.name.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(cpp_parser::Dialect::Gnu),
            Vec::<String>::new(),
            "under GNU `__int128` is a type, so `int __int128;` has no declarator"
        );
        assert_eq!(
            names(cpp_parser::Dialect::Msvc),
            vec!["__int128".to_string()],
            "and under MSVC the same text declares a variable called `__int128`"
        );
    }

    /// Every region's branches, as `kind condition body-start..body-end`, for a short assertion.
    ///
    /// The regions of a summary used to be spans and nothing else, and a span of a *condition* is not a
    /// question any layer could answer. This is the shape that replaced it: what each branch asks, and which
    /// text it guards.
    fn region_shapes(source: &str) -> Vec<Vec<String>> {
        let summary = summary(source);

        summary
            .guards
            .conditionals
            .iter()
            .map(|conditional| {
                conditional
                    .branches
                    .iter()
                    .map(|branch| {
                        format!(
                            "{:?} {} {}..{}",
                            branch.kind,
                            branch.condition.as_deref().unwrap_or("-"),
                            branch.body.start_offset,
                            branch.body.end_offset()
                        )
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_summary_stores_what_each_conditional_asks() {
        // The three spellings a condition comes in, and the one that has none. `#ifdef`/`#ifndef` store the
        // *name*, `#if` stores the expression's text — read back with the lexer the evaluator will use — and
        // `#else` stores nothing, which is not the same as storing an empty expression.
        let shapes = region_shapes(
            "#if defined(_WIN32)\n#include <a.h>\n#elif __cplusplus >= 201703L\n#include <b.h>\n#else\n#include <c.h>\n#endif\n",
        );

        assert_eq!(shapes.len(), 1, "one conditional, three branches");
        let branches = &shapes[0];
        assert_eq!(branches.len(), 3);
        assert!(
            branches[0].starts_with("If defined ( _WIN32 )"),
            "the expression's tokens, joined: {}",
            branches[0]
        );
        assert!(
            branches[1].starts_with("Elif __cplusplus >= 201703L"),
            "a version test is stored as the text a value can be compared against: {}",
            branches[1]
        );
        assert!(
            branches[2].starts_with("Else -"),
            "`#else` asks nothing: {}",
            branches[2]
        );

        // And the bodies are where the reader would put them: each branch's text is between its own directive
        // and the *start of the next one*, which is where the `#endif` is — the directive itself is not body.
        let source = "#ifdef A\n#include <a.h>\n#endif\n";
        let shapes = region_shapes(source);
        assert_eq!(
            shapes[0][0],
            format!(
                "Ifdef A {}..{}",
                source.find("#ifdef").expect("the directive") + "#ifdef A\n".len(),
                source.find("#endif").expect("the #endif")
            ),
            "the `#ifdef` branch guards everything between its directive and the `#endif`"
        );
    }

    #[test]
    fn a_regions_parent_is_the_conditional_it_is_written_inside() {
        // A nested region's condition lies inside its parent's body, and so does the text of a sibling branch —
        // which is why the nesting is stored rather than derived from the spans.
        let nested = summary("#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n");
        let parents: Vec<Option<u32>> = nested
            .guards
            .conditionals
            .iter()
            .map(|conditional| conditional.parent)
            .collect();
        assert_eq!(parents, [None, Some(0)]);

        // …and an `#elif` does not open a second region: the region *is* the conditional, and its branches are
        // the chain written inside it.
        let chained =
            summary("#ifdef A\nint a;\n#elif defined(B)\nint b;\n#else\nint c;\n#endif\n");
        assert_eq!(chained.guards.conditionals.len(), 1);
        assert_eq!(chained.guards.conditionals[0].branches.len(), 3);
        assert!(chained.guards.conditionals[0].exhaustive());
    }

    #[test]
    fn the_code_a_position_is_in_is_found_by_walking_outwards() {
        // `conditions_at` is what a fact's guard index cannot say on its own: a fact records the *innermost*
        // region, and whether the code is compiled is a question about every region around it.
        let source = "#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n";
        let summary = summary(source);
        let x = source.find("int x;").expect("the declaration");

        let chain = summary.guards.conditions_at(x);
        assert_eq!(
            chain.iter().map(|at| at.region).collect::<Vec<_>>(),
            [1, 0],
            "innermost first"
        );
        assert!(
            chain
                .iter()
                .all(|at| at.condition_at < x),
            "and each condition is evaluated at its *own* offset, which is above the code: {chain:?}"
        );

        // Code outside every conditional is in no region at all — the common case, and the cheap one.
        let source = "int x;\n#ifdef A\nint y;\n#endif\n";
        assert!(summary
            .guards
            .conditions_at(source.find("int x;").expect("the declaration"))
            .is_empty());
    }

    #[test]
    fn a_position_is_looked_up_in_the_branch_it_sits_in() {
        // The branch in force is what decides whether the code is compiled, and it is a question about the
        // position — `#else` is not "the whole region", it is one branch of it.
        let source = "#ifdef A\nint taken;\n#else\nint not_taken;\n#endif\n";
        let summary = summary(source);
        let region = summary.guards.conditionals.first().expect("one region");

        let first = source.find("int taken").expect("the first branch");
        let second = source.find("int not_taken").expect("the second branch");
        assert_eq!(region.branch_at(first), Some(0));
        assert_eq!(region.branch_at(second), Some(1));
    }

    #[test]
    fn the_chain_a_guard_names_is_the_chain_the_position_is_in() {
        // Two ways to the same answer: from the guard a fact carries (each region names its parent, so this is a
        // walk outwards), and from the position (a search over every region's span). They must agree — the guard
        // is the innermost region the sweep found, and the spans are that same sweep's arithmetic. Where they do
        // not, the file's directives do not balance and the structure is two readings of one broken file; the
        // guard is the one the rest of the index is consistent with.
        for source in [
            "#ifdef A\nint x;\n#endif\n",
            "#ifdef A\n#ifdef B\nint x;\n#endif\n#endif\n",
            "#ifdef A\nint a;\n#elif defined(B)\nint b;\n#else\nint c;\n#endif\n",
            "#ifndef GUARD\n#define GUARD\n#ifdef A\nint x;\n#endif\n#endif\n",
            "int early;\n#if defined(A) && defined(B)\n#ifdef C\nint x;\n#else\nint y;\n#endif\n#endif\n",
        ] {
            let summary = summary(source);

            for (region, conditional) in summary.guards.conditionals.iter().enumerate() {
                for branch in &conditional.branches {
                    // Anywhere inside the branch's body is a position whose guard names this region — unless a
                    // *nested* region is there, which is what the chain has to account for either way.
                    if branch.body.length == 0 {
                        continue;
                    }

                    let at = branch.body.start_offset;
                    assert_eq!(
                        summary.guards.conditions_at(at),
                        summary.guards.conditions_of(region as u32),
                        "{source:?} at {at} (region {region})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_definition_carries_a_value_exactly_when_a_condition_could_read_one() {
        // `#if NAME` expands the name and reads the result as a number, and the evaluator accepts **one integer
        // literal** — so the fact stores a value in exactly that case and nothing else. A name defined to a name,
        // to an expression, to nothing, or as a function-like macro is `Unknown` to a condition, and a spelling no
        // evaluator can use would only make the cache bigger.
        let value_of = |source: &str, name: &str| {
            summary(source)
                .macros
                .iter()
                .find(|fact| fact.name == name)
                .and_then(|fact| fact.value.clone())
        };

        assert_eq!(value_of("#define ABI 1\n", "ABI").as_deref(), Some("1"));
        assert_eq!(
            value_of("#define WIDE 0x10UL\n", "WIDE").as_deref(),
            Some("0x10UL"),
            "the spelling is kept as written: the evaluator is what reads it"
        );
        assert_eq!(value_of("#define NAME other\n", "NAME"), None);
        assert_eq!(value_of("#define EXPR 1 + 2\n", "EXPR"), None);
        assert_eq!(value_of("#define EMPTY\n", "EMPTY"), None);
        assert_eq!(value_of("#define CALL(x) x\n", "CALL"), None);
        assert_eq!(
            value_of("#undef GONE\n", "GONE"),
            None,
            "an `#undef` says the name stops being a macro, which is all it says"
        );
    }

    /// The facts about `name`, with their guards and the settling flag, for a short assertion.
    fn macro_facts(source: &str, name: &str) -> Vec<(crate::summary::MacroKind, FactGuard, bool)> {
        summary(source)
            .macros
            .iter()
            .filter(|fact| fact.name == name)
            .map(|fact| (fact.kind, fact.guard, fact.settles_the_name))
            .collect()
    }

    #[test]
    fn a_define_inside_ifndef_settles_the_name_whatever_the_branch() {
        // The idiom the whole flag exists for, and the shape the system headers define most macros in:
        // `#ifndef NAME / #define NAME`. Taken → defined here; not taken → it was defined already. Either way the
        // name is a macro, so a *use* after this block is a use and not a "maybe".
        //
        // A declaration before the conditional is what keeps it from being the **file's own guard** — which is a
        // separate rule that would make the fact `Unconditional` instead (see the test after this one).
        assert_eq!(
            macro_facts("int early;\n#ifndef NAME\n#define NAME 1\n#endif\n", "NAME"),
            [(MacroKind::Definition, FactGuard::Region(0), true)]
        );
    }

    #[test]
    fn the_files_own_guard_is_not_a_condition_and_neither_is_a_define_inside_it() {
        // `#ifndef WIDGET_H / #define WIDGET_H` wrapping the file is a guard, so its facts are unconditional
        // already — and a `#ifndef NAME` nested inside one inherits that: reaching the file at all is what the
        // guard means, which is the same rule the de-guard step applies to declarations and includes.
        assert_eq!(
            macro_facts(
                "#ifndef WIDGET_H\n#define WIDGET_H\n#ifndef NAME\n#define NAME 1\n#endif\n#endif\n",
                "NAME"
            ),
            [(MacroKind::Definition, FactGuard::Region(1), true)]
        );
        assert_eq!(
            macro_facts("#ifndef WIDGET_H\n#define WIDGET_H\n#endif\n", "WIDGET_H"),
            [(MacroKind::Definition, FactGuard::Unconditional, false)]
        );
    }

    #[test]
    fn a_region_whose_every_branch_agrees_settles_the_name() {
        // The general case behind the idiom: whichever branch ran, it did the same thing to the name. The two
        // definitions may differ in what they define it *as* — that is the question the flag deliberately does not
        // answer, and `macro_definition` therefore still ignores it.
        assert_eq!(
            macro_facts(
                "int early;\n#if defined(A)\n#define NAME 1\n#else\n#define NAME 2\n#endif\n",
                "NAME"
            ),
            [
                (MacroKind::Definition, FactGuard::Region(0), true),
                (MacroKind::Definition, FactGuard::Region(0), true)
            ]
        );

        // `#ifdef NAME / #undef NAME` is the mirror: after it the name is certainly not a macro.
        assert_eq!(
            macro_facts("int early;\n#ifdef NAME\n#undef NAME\n#endif\n", "NAME"),
            [(MacroKind::Undefinition, FactGuard::Region(0), true)]
        );
    }

    #[test]
    fn a_region_that_does_not_settle_the_name_marks_nothing() {
        // The four shapes that must stay false, because a rule that over-claims is worse than no rule at all: a
        // single branch that may not be taken; branches that disagree; a branch whose last word on the name is the
        // opposite of what it started with; and a `#define` in a branch that the condition does not name.
        for source in [
            // No `#else`: "nothing ran" is a possibility, and then the name may not be a macro.
            "int early;\n#if defined(A)\n#define NAME 1\n#endif\n",
            // Branches that disagree about the name.
            "int early;\n#if defined(A)\n#define NAME 1\n#else\n#undef NAME\n#endif\n",
            // The branch's *last* word is `#undef`, so the branch leaves the name undefined.
            "int early;\n#ifndef NAME\n#define NAME 1\n#undef NAME\n#endif\n",
            // The condition is not about this name, so nothing about it is settled.
            "int early;\n#ifndef OTHER\n#define NAME 1\n#endif\n",
            // A name defined in only one of two exhaustive branches: the other one leaves it as it found it.
            "int early;\n#if defined(A)\n#define NAME 1\n#else\n#define OTHER 2\n#endif\n",
        ] {
            let facts = macro_facts(source, "NAME");
            assert!(
                facts.iter().all(|(_, _, settles)| !settles),
                "{source:?} must settle nothing, got {facts:?}"
            );
        }
    }

    #[test]
    fn a_settling_region_inside_a_plain_conditional_settles_nothing() {
        // The chain rule, and the case it exists for: if `A` is false the inner `#ifndef NAME` never runs, so
        // nothing about the name is certain — the outer region does not settle it either.
        assert_eq!(
            macro_facts(
                "int early;\n#if defined(A)\n#ifndef NAME\n#define NAME 1\n#endif\n#endif\n",
                "NAME"
            ),
            [(MacroKind::Definition, FactGuard::Region(1), false)]
        );
    }

    #[test]
    fn a_settling_region_reached_through_an_if_defined_settles_the_name() {
        // The shape the system headers actually use: `#if !defined(NAME)` written out instead of `#ifndef NAME`,
        // and wrapped in parentheses for good measure. Recognised as the same claim, because it *is* the same
        // claim — the condition is about the very name the body writes.
        for condition in ["!defined(NAME)", "!defined NAME", "(!defined(NAME))"] {
            let source = format!("int early;\n#if {condition}\n#define NAME 1\n#endif\n");
            assert_eq!(
                macro_facts(&source, "NAME"),
                [(MacroKind::Definition, FactGuard::Region(0), true)],
                "{condition} should settle the name"
            );
        }
    }

    #[test]
    fn a_file_whose_conditionals_do_not_balance_settles_nothing() {
        // The safety net, and the reason it exists: the rule reads a *nesting*, and the nesting comes from the
        // directives the parser found. A file with syntax errors can lose one — measured, `winnt.h` loses eight
        // `#endif`s and 417 errors is what it costs — and a parent chain that is wrong in the shorter direction
        // would claim a name is settled when an enclosing `#if` says it may not be. So an unbalanced file gets no
        // claims at all, in either direction.
        for source in [
            // A region never closed: the `#ifndef NAME` may be inside something that is not taken.
            "int early;\n#if defined(A)\n#ifndef NAME\n#define NAME 1\n#endif\n",
            // An `#endif` that closes nothing: an opener is missing, and the depth after it is wrong.
            "int early;\n#endif\n#ifndef NAME\n#define NAME 1\n#endif\n",
        ] {
            let facts = macro_facts(source, "NAME");
            assert!(
                facts.iter().all(|(_, _, settles)| !settles),
                "{source:?} must settle nothing, got {facts:?}"
            );
        }
    }

    #[test]
    fn a_file_with_declarations_and_macros_summarizes_both() {        let summary = summary(
            "#define MY_API\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\nstruct Widget { int size; };\n",
        );

        assert_eq!(summary.declarations.len(), 2, "the class and its member");
        assert_eq!(summary.macros.len(), 2, "both macros");

        let api = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MY_API")
            .expect("the object-like macro is a fact");
        assert!(!api.function_like);
        assert_eq!(api.body, MacroBody::Unknown, "an empty body has no shape");

        let max = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MAX")
            .expect("the function-like macro is a fact");
        assert!(max.function_like);
        assert_eq!(
            max.body,
            MacroBody::Expression,
            "the body is one parenthesised group"
        );
    }

    #[test]
    fn a_declaration_inside_the_files_own_guard_is_unconditional() {
        // The rule that makes a header's contents visible at all: `#ifndef H` … `#endif` around the whole file is
        // not a condition anybody has to decide, because *including the file* is what defines `H`. Measured on the
        // closure of `<string>`, without this rule every cross-file answer about the standard library is
        // `ConditionalCompilation` — the headers guard their bodies, so every `#include` inside one is "guarded".
        let summary =
            summary("#ifndef H\n#define H\nstruct Widget { int size; };\n#include \"other.h\"\n#endif\n");

        let widget = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "Widget")
            .expect("the guarded declaration is a fact");
        assert_eq!(
            widget.guard,
            FactGuard::Unconditional,
            "the file's own guard is entered by including the file at all"
        );

        let include = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "other.h")
            .expect("the include is a fact");
        assert_eq!(
            include.guard,
            FactGuard::Unconditional,
            "and so is an `#include` written inside it"
        );

        assert_eq!(
            summary.guards.regions.len(),
            1,
            "the region is still recorded: the `#if` is a fact about the text either way"
        );
    }

    #[test]
    fn a_conditional_that_is_not_the_files_guard_still_guards() {
        // What the rule must not swallow: `#if defined(A)` is a real condition, and a fact inside it is still
        // `Unknown` to a consumer that cannot evaluate it.
        let summary = summary("#if defined(A)\nint inside;\n#endif\n");

        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the declaration is a fact");
        assert_eq!(fact.guard, FactGuard::Region(0));
    }

    #[test]
    fn a_header_that_declares_something_before_its_guard_is_not_guarded_by_it() {
        // `detect_guard` refuses a guard that does not wrap the whole file, and the rule follows it rather than
        // making up its own answer: the `#ifndef` here is an ordinary conditional and stays one.
        let summary = summary("int early;\n#ifndef H\n#define H\nint inside;\n#endif\n");

        let early = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "early")
            .expect("the declaration before the guard is a fact");
        assert_eq!(
            early.guard,
            FactGuard::Unconditional,
            "it is outside every region, which is a different answer from the guard's"
        );

        let inside = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the declaration inside is a fact");
        assert_eq!(
            inside.guard,
            FactGuard::Region(0),
            "a guard that does not wrap the file does not de-guard what it does contain"
        );
    }

    #[test]
    fn a_declaration_in_a_conditional_keeps_its_region_through_the_builder() {
        let summary = summary("#if defined(A)\nint inside;\n#endif\n");

        assert_eq!(summary.guards.regions.len(), 1);
        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "inside")
            .expect("the guarded declaration is a fact");
        assert_eq!(fact.guard, FactGuard::Region(0));
    }

    #[test]
    fn an_include_that_cannot_be_resolved_keeps_its_spelling() {
        let summary = summary("#include <vector>\n#include \"local.h\"\n");

        assert_eq!(summary.includes.len(), 2);

        let vector = &summary.includes[0];
        assert_eq!(vector.form, IncludeForm::Angle);
        assert_eq!(vector.spelling, "vector");
        assert_eq!(
            vector.resolved, None,
            "with nothing to search, the include is unresolved rather than dropped"
        );

        assert_eq!(summary.includes[1].form, IncludeForm::Quote);
        assert_eq!(summary.includes[1].spelling, "local.h");
    }

    #[test]
    fn an_include_resolves_against_the_provider() {
        let files = MemoryFiles::new()
            .with_file("/p/local.h", "int x;\n")
            .with_file("/usr/include/vector", "// the real one\n");

        // An *angle* include searches the configured paths and not the including file's directory, so the
        // configuration is what makes it resolvable — a distinction worth a test, because a resolver that
        // searched the local directory for `<vector>` would find a project's own file of that name and shadow
        // the standard header.
        let mut config = CompilerConfig::default();
        config.include_paths = vec![crate::include::config::IncludePath::system("/usr/include")];

        let indexer = FileIndexer::new(&files, &config);
        let summary = indexer.index(
            Path::new("/p/widget.cpp"),
            "#include <vector>\n#include \"local.h\"\n",
            key(),
        );

        let vector = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "vector")
            .expect("the angle include is a fact");
        assert_eq!(
            vector.resolved.as_deref(),
            Some(Path::new("/usr/include/vector")),
            "an angle include searched the configured path"
        );

        let local = summary
            .includes
            .iter()
            .find(|fact| fact.spelling == "local.h")
            .expect("the quoted include is a fact");
        assert_eq!(
            local.resolved.as_deref(),
            Some(Path::new("/p/local.h")),
            "a quoted include looks beside the including file first"
        );
    }

    #[test]
    fn every_summary_survives_being_written_and_read_back() {
        let directory = std::env::temp_dir().join("cppls-summary-round-trip");
        let _ = std::fs::remove_dir_all(&directory);

        let summary = summary(
            "#define MY_API\n#if defined(A)\nstruct Widget { int size; };\n#endif\n#include <vector>\n",
        );
        let written = write_summary(&summary, &directory).expect("the write must succeed");

        assert!(
            written.starts_with(&directory),
            "the summary lands under the project root: {written:?}"
        );

        let read = super::read_summary(&written).expect("what was written must be readable");
        assert_eq!(read, summary);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_macro_body_classifier_reads_each_shape() {
        let cases = [
            ("#define A __declspec(dllexport)", MacroBody::Specifier),
            ("#define A extern \"C\"", MacroBody::Specifier),
            ("#define A do { } while (false)", MacroBody::Statement),
            ("#define A if (x) { }", MacroBody::Statement),
            ("#define A return", MacroBody::Statement),
            ("#define A ((a) > (b) ? (a) : (b))", MacroBody::Expression),
            ("#define A (a)", MacroBody::Expression),
            ("#define A int", MacroBody::Type),
            ("#define A unsigned long", MacroBody::Type),
            ("#define A { }", MacroBody::Block),
            ("#define A", MacroBody::Unknown),
            ("#define A something_else + 1", MacroBody::Unknown),
        ];

        for (source, expected) in cases {
            let tree = CppParser::parse(source, ParserConfig::default());
            let preprocessing = preprocess(&tree.get_red_root());

            let Directive::Define(define) = &preprocessing.directives[0].directive else {
                panic!("{source:?} must be a define");
            };
            let definition = define.macro_def.as_ref().expect("a readable definition");

            assert_eq!(
                macro_body_shape(definition),
                expected,
                "{source:?} should be {expected:?}"
            );
        }
    }

    #[test]
    fn a_body_that_is_not_one_parenthesised_group_is_not_an_expression() {
        // `(a) + (b)` is a sum: it starts with `(` and ends with `)` and the group closes before the end. Calling
        // it an expression would be right in this case and wrong for a statement that happens to be written that
        // way, so the answer is `Unknown` — which still rules out the call reading.
        let tree = CppParser::parse("#define A (a) + (b)", ParserConfig::default());
        let preprocessing = preprocess(&tree.get_red_root());

        let Directive::Define(define) = &preprocessing.directives[0].directive else {
            panic!("must be a define");
        };
        let definition = define.macro_def.as_ref().expect("a readable definition");

        assert_eq!(macro_body_shape(definition), MacroBody::Unknown);
    }

    #[test]
    fn an_unresolved_include_does_not_stop_the_other_facts() {
        let tree = CppParser::parse("#include <nonexistent_xyz>\nstruct W { int a; };\n", ParserConfig::default());
        let preprocessing = preprocess(&tree.get_red_root());

        let files = MemoryFiles::new();
        let config = CompilerConfig::default();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(cfg!(windows));

        let facts: Vec<_> = preprocessing
            .directives
            .iter()
            .filter_map(|spanned| {
                include_fact(
                    &spanned.directive,
                    spanned.range,
                    Path::new("/p"),
                    &resolver,
                    &mut interner,
                )
            })
            .collect();

        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].spelling, "nonexistent_xyz");
        assert_eq!(facts[0].resolved, None);
    }

    #[test]
    fn a_macro_fact_points_at_the_name_not_the_directive() {
        let summary = summary("  #define   MY_API   int\n");

        let fact = summary
            .macros
            .iter()
            .find(|fact| fact.name == "MY_API")
            .expect("the macro is a fact");

        // The name is what a rename edits and what a "go to definition" puts the cursor on.
        let text = "  #define   MY_API   int\n";
        assert_eq!(
            &text[fact.range.start_offset..fact.range.end_offset()],
            "MY_API"
        );
    }

    #[test]
    fn a_summary_carries_no_resolved_conclusions() {
        // The first invariant of `docs/index-design.md`, asserted rather than assumed: a summary holds names as
        // written and nothing that would have to be recomputed when a header changes.
        let summary = summary("namespace ns { struct Widget { int member; }; }\n");

        let widget = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "Widget")
            .expect("the class is a fact");

        assert_eq!(widget.kind, DeclKind::Type);
        assert_eq!(
            widget.scope.as_deref(),
            Some("ns"),
            "the scope is the spelling the file used, not a resolved identity"
        );
        assert_eq!(widget.qualified_name(), "ns::Widget");
    }

    /// A provider with one file, for the resolution test above.
    struct OneFile(PathBuf, String);

    impl FileProvider for OneFile {
        fn read(&self, path: &Path) -> Option<String> {
            (path == self.0).then(|| self.1.clone())
        }

        fn exists(&self, path: &Path) -> bool {
            path == self.0
        }
    }

    #[test]
    fn a_provider_that_has_one_file_still_answers_for_it() {
        let files = OneFile(PathBuf::from("/p/only.h"), "int x;\n".to_string());
        let table = MacroTable::new();
        let _ = table;

        assert_eq!(files.read(Path::new("/p/only.h")).as_deref(), Some("int x;\n"));
        assert_eq!(files.read(Path::new("/p/other.h")), None);
    }
}
