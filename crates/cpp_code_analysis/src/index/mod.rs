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

pub mod project;
pub mod store;
pub mod watch;
pub mod worklist;

pub use project::{
    IncludeVisibility, ProjectDefinition, ProjectIndex, ProjectMacro, VisibleDeclaration,
    definition_across_files, macro_across_files,
};
pub use store::{StoreStats, SummaryStore};
pub use watch::{ChangeBatch, EventKind, FileEvent, Response, WatchFilter};
pub use worklist::{Priority, Step, StepOutcome, Worklist};

use crate::cache::{SummaryKey, content_hash};
use crate::include::config::CompilerConfig;
use crate::include::paths::{FileProvider, PathInterner};
use crate::include::IncludeResolver;
use crate::preprocess::directive::{Directive, SpannedDirective};
use crate::preprocess::preprocess;
use crate::sema::declarations::{assign_guards, build_facts};
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
        let tree = CppParser::parse(source, ParserConfig::default());
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
        let (declarations, mut guards) = build_facts(&scopes, &preprocessing, &root);

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
                // The *name's* range, not the directive's: "go to macro definition" is a jump to the name a user
                // can see, and a rename edits it. The guard sweep reads this fact's offset too, and the name is
                // inside the same conditional region as the directive that wrote it — a `#define` cannot span an
                // `#endif`.
                range: definition.name_range,
                guard: crate::summary::FactGuard::Unconditional,
            })
        }
        Directive::Undef { name: Some(name) } => Some(MacroFact {
            name: name.to_string(),
            kind: crate::summary::MacroKind::Undefinition,
            function_like: false,
            // Nothing to say about a body, and saying `Unknown` is the honest way to say it.
            body: cpp_parser::MacroBody::Unknown,
            range: spanned.range,
            guard: crate::summary::FactGuard::Unconditional,
        }),
        _ => None,
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
    use crate::summary::{DeclKind, FactGuard};
    use cpp_parser::{CppParser, MacroBody, ParserConfig};
    use std::path::{Path, PathBuf};

    fn key() -> SummaryKey {
        SummaryKey::new(0, 0)
    }

    fn summary(source: &str) -> crate::summary::FileSummary {
        summarize(Path::new("/p/widget.cpp"), source, key())
    }

    #[test]
    fn a_file_with_declarations_and_macros_summarizes_both() {
        let summary = summary(
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
