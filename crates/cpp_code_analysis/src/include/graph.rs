//! Following includes: the dependency graph, and the visits that build it.
//!
//! # What the graph is for
//!
//! A **reverse** edge is the only way to answer "what does this edit affect?". Without it, changing a header
//! means discarding every analysis in the project, because nothing records which files read it — and
//! discarding everything is the same as having no cache. So [`crate::graph::FileGraph::dependents_of`] is
//! the point of this module, and the forward edges exist to compute it.
//!
//! # Why a header cannot be analysed once and reused
//!
//! `#include` is textual, so the macros in force when a header is read depend on *who included it*: a
//! header reached from two places can see two different macro tables, and its `#if` conditions can go
//! different ways. The graph therefore records an edge per *include*, not per file pair, and each edge
//! carries what the visit decided.
//!
//! # What a guard does here
//!
//! A guarded header is read once — that is what the guard is for — and a second edge to it is marked
//! [`Visit::Skipped`] rather than followed. An **unguarded** header is not: a compiler reads it again every
//! time and re-defines everything in it, and so does this, because pretending otherwise would hide the
//! duplicate definitions that are the reason guards get written.
//!
//! # What this deliberately does not do
//!
//! It does not reconstruct a position-exact macro table for every byte of every file. What a consumer asks
//! at this stage is "is this file included, and did its guard take effect", and the conditions *inside* a
//! file are already carried by the [`crate::guard::Guard`] on each declaration. Building a table per file is
//! a per-keystroke cost nothing needs yet.
//!
//! # What happens when the context is not there
//!
//! A header is written to be included, so its conditions are about macros its **includer** supplies — and when
//! a user opens a header directly there is no includer in the analysis. The macro environment is then
//! genuinely unknown, and the walk says so rather than guessing: the file is recorded with
//! [`FileEntry::missing_context`], and names nothing in the walk has seen become *unknown* instead of
//! undefined, so an `#ifdef` on them neither kills the code behind it nor hides the includes in it.
//!
//! Both directions of the guess are wrong in their own way and they are not equally wrong. Calling an
//! unknown context complete reports errors in code that compiles; calling a complete one unknown only makes
//! the analysis quieter. So the flag errs the second way, and a consumer can always tell "no macros are
//! defined here" apart from "the macros that are defined here are not all of them".

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cpp_parser::CppSyntaxTree;

use crate::{
    config::CompilerConfig,
    directive::{Directive, DirectiveKind, IncludeForm, SpannedDirective},
    guards::{Guard, analyse_guards},
    include::{FoundIn, IncludeResolver, Resolution},
    paths::{FileId, FileProvider, PathInterner, parent_normalized},
};

/// How deep includes are followed before the walk gives up.
///
/// A real limit rather than a formality: include cycles exist in real code and are broken by guards rather
/// than by the graph, so a walk that trusted the graph alone would not terminate. Depth also bounds the
/// *work*: a header twenty levels down is not worth re-analysing for a keystroke.
pub const MAX_INCLUDE_DEPTH: usize = 64;

/// Why an include was not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The file is already on the path being walked, so following it would not terminate.
    Cycle,
    /// Reached deeper than [`MAX_INCLUDE_DEPTH`].
    TooDeep,
    /// The file guards itself and has been read already, so a compiler would not read it again either.
    AlreadyVisited,
    /// The walk was asked to look at one file only, so this include was recorded and not read.
    ///
    /// Distinct from the other reasons on purpose: they say "reading this would be wrong or pointless", while
    /// this one says "reading this was not asked for". A consumer showing why a declaration is missing needs
    /// to tell those apart, because only one of them is a fact about the code.
    OutOfScope,
}

/// What happened at the other end of an include.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visit {
    /// The file was read and its own includes followed.
    Analysed,
    /// The file was not read again.
    Skipped(SkipReason),
}

/// An `#include` that resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: FileId,
    pub to: FileId,
    /// The name as written, so a consumer can show the include rather than the resolved path.
    pub name: Box<str>,
    pub is_angle: bool,
    /// Which search step found it — what `#include_next` needs, and what distinguishes a project header
    /// from a system one.
    pub found_in: FoundIn,
    pub visit: Visit,
    /// Where the directive is.
    pub range: cpp_parser::SourceRange,
}

/// An `#include` that did not resolve, and where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedInclude {
    pub from: FileId,
    /// The name as written, without delimiters.
    pub name: Box<str>,
    /// Every candidate that was tried, in order — so a "cannot find header" message can say where it
    /// looked, which is the difference between an actionable message and a useless one.
    pub searched: Vec<PathBuf>,
    pub range: cpp_parser::SourceRange,
}

/// A file the walk reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    /// How the file protects itself.
    pub guard: Guard,
    /// The macros it defines, in order.
    ///
    /// Kept even when the visit was skipped, because they are what the skip is *about*: a guarded header
    /// defines its guard, and that definition is why the second visit does nothing.
    pub defines: Vec<Box<str>>,
    /// How deeply it was first reached; `0` for the file the walk started from.
    pub depth: usize,
    /// The macro names in force **when this file was entered**.
    ///
    /// This is the cross-file chain made observable. `#include` is textual, so a header reached from two
    /// places can see two different sets of macros — and its `#if` conditions can go different ways. A
    /// consumer looking up the macros that were in force at this file's first line has the answer here
    /// instead of re-walking the graph to find out.
    ///
    /// Names, not definitions: what a condition asks is whether a name is defined, and a name's *value* is
    /// [`crate::condition`]'s business once expansion has run. The set is sorted, so two runs of the walk
    /// can be compared.
    pub macros: Vec<Box<str>>,
    /// Whether macros that could not be accounted for might still have been in force here.
    ///
    /// True for a header the walk started from, because its includer — the translation unit that supplies the
    /// `-D`s and the `#define`s coming before it — is not part of this walk and cannot be guessed. True also
    /// when the walk was asked not to follow includes ([`WalkScope::FileOnly`]), since then nothing outside
    /// the file was consulted either. False for a translation unit walked normally, whose macro environment
    /// really does start at the command line.
    ///
    /// The distinction is not pedantry, and the two wrong answers are not equally wrong:
    ///
    /// * Say a header's environment is **complete** when it is not, and every macro the includer defines
    ///   reads as undefined. `#ifdef _WIN32` goes the wrong way, and `#if 0`-style dead code is invented out
    ///   of a condition that was never false — which is how a consumer ends up reporting errors in code that
    ///   compiles.
    /// * Say a translation unit's environment is **incomplete** when it is complete, and nothing is decided
    ///   that could have been. Diagnostics get quieter, which is recoverable, and completion gets noisier,
    ///   which is merely unhelpful.
    ///
    /// So the second mistake is the one to make when the answer is genuinely unknown, and the flag is what
    /// lets a consumer tell "no macros are defined here" apart from "the macros that are defined here are not
    /// all of them".
    pub missing_context: bool,
}

impl FileEntry {
    /// Can conditions in this file be decided against the macros recorded here?
    ///
    /// `false` for a header analysed on its own: an `#ifdef` that looks false may be true in the translation
    /// unit that includes it, so a consumer must not drop what it guards — see [`FileEntry::missing_context`].
    pub fn context_is_complete(&self) -> bool {
        !self.missing_context
    }

    /// Is this file's own contents all the analysis had to go on?
    ///
    /// The state a consumer has to handle, and the reason it is a question about the *file* rather than about
    /// the walk: a header whose includer is in the walk is analysed with whatever included it, so its macros
    /// are the ones a compiler would use and no caveat is needed even though the walk elsewhere is partial.
    ///
    /// See [`crate::graph::file_only`] for the file-only analysis this describes.
    pub fn is_file_only(&self) -> bool {
        self.depth == 0 && self.missing_context
    }
}

/// The result of walking one translation unit.
#[derive(Debug, Clone, Default)]
pub struct FileGraph {
    /// Every file reached, indexed by `FileId`. A `FileId` the walk did not reach leaves a placeholder.
    pub files: Vec<FileEntry>,
    /// Every resolved include, in the order the walk reached it.
    pub edges: Vec<Edge>,
    /// Every include that could not be resolved.
    pub unresolved: Vec<UnresolvedInclude>,
    /// The files that were read, in the order they were entered.
    pub analysed: Vec<FileId>,
}

impl FileGraph {
    /// Where a file is, if the walk reached it.
    ///
    /// `None` for a file the interner knows but this walk never reached: the interner is shared and may
    /// have minted ids for other walks, so an id is not a promise that there is an entry.
    pub fn entry(&self, file: FileId) -> Option<&FileEntry> {
        self.files.get(file.index())
    }

    /// Where a file is, if it was read.
    pub fn path(&self, file: FileId) -> Option<&Path> {
        self.entry(file).map(|entry| entry.path.as_path())
    }

    /// The files that include `file`, directly.
    ///
    /// The reverse edge, and the reason the graph exists: an edit to a header invalidates exactly these, and
    /// transitively whatever includes them.
    pub fn includers_of(&self, file: FileId) -> Vec<FileId> {
        let mut includers: Vec<FileId> = self
            .edges
            .iter()
            .filter(|edge| edge.to == file)
            .map(|edge| edge.from)
            .collect();
        includers.sort_unstable();
        includers.dedup();
        includers
    }

    /// The files `file` includes, directly.
    pub fn includes_of(&self, file: FileId) -> Vec<FileId> {
        let mut includes: Vec<FileId> = self
            .edges
            .iter()
            .filter(|edge| edge.from == file)
            .map(|edge| edge.to)
            .collect();
        includes.sort_unstable();
        includes.dedup();
        includes
    }

    /// Every file whose analysis an edit to `file` could change, transitively — `file` included.
    ///
    /// The closure a cache invalidation needs. `file` itself is included because editing a file invalidates
    /// its own analysis, and the result is sorted so that a caller can compare two runs.
    pub fn dependents_of(&self, file: FileId) -> Vec<FileId> {
        let mut seen: HashSet<FileId> = HashSet::new();
        let mut pending = vec![file];

        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            pending.extend(self.includers_of(current));
        }

        let mut result: Vec<FileId> = seen.into_iter().collect();
        result.sort_unstable();
        result
    }

    /// The files this walk read.
    pub fn analysed_files(&self) -> &[FileId] {
        &self.analysed
    }

    /// The includes that did not resolve.
    pub fn unresolved_includes(&self) -> &[UnresolvedInclude] {
        &self.unresolved
    }
}

/// Where the walk has been.
///
/// The path and the visited set are separate questions and both are needed: a file on the path is a cycle
/// whatever its guard says, while a file merely visited is a cycle only if it is guarded.
#[derive(Debug, Default)]
struct WalkState {
    visited: HashSet<FileId>,
    on_path: Vec<FileId>,
    /// Whether the macro environment the walk started from was incomplete — see [`walk`].
    ///
    /// Part of the walk's state rather than of each call's arguments because it is a property of the whole
    /// walk and never changes once it starts: every file this walk reaches is reached from the same
    /// environment, so a header reached from a header opened on its own carries the same caveat.
    root_missing_context: bool,
    /// Whether includes are followed at all — see [`WalkScope`].
    follows_includes: bool,
}

impl WalkState {
    /// Must a file reached by this walk report an incomplete macro context?
    ///
    /// One place rather than a condition repeated at each entry constructed, because the two reasons are easy
    /// to get half-right: a header opened on its own has no includer to ask, and a file-only walk consulted
    /// nothing outside the file it was given. Both leave macros unaccounted for, and a consumer's behaviour
    /// depends on being told.
    fn missing_context(&self) -> bool {
        self.root_missing_context || !self.follows_includes
    }
}

/// How much of the project a walk is allowed to read.
///
/// The two answers are not degrees of the same thing; they are answers to different questions, and which one
/// is right depends on what the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WalkScope {
    /// Follow `#include`s and analyse every file reached, inheriting macros along the way.
    ///
    /// What a `.cpp` gets, and what a header gets once some translation unit that includes it is known: this
    /// is the analysis that can answer "which configuration is this declaration compiled under", because it
    /// is the only one that has the macros.
    #[default]
    FollowIncludes,
    /// Analyse **only** the file named, treating its `#include`s as edges to record and nothing more.
    ///
    /// The fallback for a header nothing includes, and the one thing that is always safe to do: everything
    /// decided here is decided from what the file says about itself, so nothing outside it can make the answer
    /// wrong. A header's own `#include`s are not read, so a macro **they** would have defined is unknown
    /// rather than absent — which is exactly the caveat [`FileEntry::missing_context`] records.
    ///
    /// It is a fallback and not a preference: a file analysed this way knows less than one analysed the other
    /// way, and its conditions are unknown where the other's are decided. What it buys is that the answer is
    /// never *wrong* — there is no configuration it can be contradicted by, because it never claims to know
    /// one.
    FileOnly,
}

impl WalkScope {
    /// Does this scope read the files a file includes?
    fn follows_includes(self) -> bool {
        matches!(self, WalkScope::FollowIncludes)
    }
}

/// Follow a translation unit's includes.
///
/// `source` and `tree` are the file the walk starts from, already parsed by the caller — it usually has
/// them, and parsing twice is the most expensive thing this could do.
///
/// # A header as the root
///
/// `root_path` is usually a `.cpp`, and then everything the walk decides is decided against a complete macro
/// environment: a translation unit really does start with the command line and nothing else. A **header** as
/// the root is the editor's case — the user opened the file, and no translation unit is being built — and
/// there the environment is genuinely incomplete, because the `-D`s and the `#define`s that come before the
/// header live in whichever translation unit includes it and cannot be guessed from the header.
///
/// The walk does not pretend otherwise. It records the file as [`FileEntry::missing_context`] and still
/// follows whatever its `#include`s resolve to, because *those* files are reached with the same incomplete
/// environment and a consumer asking about them wants the same caveat rather than silence.
///
/// [`walk_scoped`] is the same walk with the choice made explicitly; use [`file_only`] when the answer wanted
/// is about one file and nothing outside it should be read.
pub fn walk<F: FileProvider>(
    source: &str,
    tree: &CppSyntaxTree,
    root_path: &Path,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
) -> FileGraph {
    walk_scoped(
        source,
        tree,
        root_path,
        files,
        config,
        interner,
        WalkScope::FollowIncludes,
    )
}

/// Follow a translation unit's includes, to the extent `scope` allows.
///
/// See [`WalkScope`] for why the choice exists. The root is analysed either way — it is the file the caller
/// handed over, so reading it is the one thing that was certainly asked for.
pub fn walk_scoped<F: FileProvider>(
    source: &str,
    tree: &CppSyntaxTree,
    root_path: &Path,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
    scope: WalkScope,
) -> FileGraph {
    let mut graph = FileGraph::default();
    let mut state = WalkState {
        // A header analysed on its own is the case where the macro environment cannot be known: it is written
        // to be included, and what it is included *by* is what decides its conditions.
        root_missing_context: is_probably_a_header(root_path),
        follows_includes: scope.follows_includes(),
        ..WalkState::default()
    };

    // The starting state carries the same caveat, which is what makes it reach the decisions: a name the
    // command line and the header itself do not mention is *unknown* rather than undefined, so an `#ifdef`
    // on it neither guards dead code nor hides it.
    //
    // A file-only walk is incomplete for a second reason and it lands in the same place: nothing outside the
    // file was read, so a name the file does not mention may well have been defined by an `#include` above —
    // and "may well have been" is precisely what the state has to say.
    let mut starting = Marked::from_config(config);
    if state.missing_context() {
        starting = starting.incomplete();
    }

    let root = interner.intern(root_path);

    // The root is read here rather than through `follow`, because the caller already has it: parsing it again
    // would be the most expensive thing this function could do. It is still an analysed file, and recording it
    // as one is not bookkeeping — `analysed_files` is what a consumer walks to do per-file work, so a root
    // missing from it means a file that is silently never analysed.
    graph.analysed.push(root);

    visit(
        source, tree, root, &starting, &mut graph, &mut state, files, config, interner, 0,
    );

    graph
}

/// Analyse **one file** and nothing it includes.
///
/// The degraded analysis for a header whose translation unit is not known — see [`WalkScope::FileOnly`]. It
/// reads `source`, reads no other file, and reports what the file says about itself: its guard, the macros it
/// defines, and its `#include` edges, which are recorded as [`Visit::Skipped`] with
/// [`SkipReason::OutOfScope`] rather than resolved. The result is the same shape as [`walk`]'s, so a consumer can swap one for the other
/// without a second code path, and every entry it produces reports
/// [`FileEntry::context_is_complete`] as `false`.
pub fn file_only<F: FileProvider>(
    source: &str,
    tree: &CppSyntaxTree,
    path: &Path,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
) -> FileGraph {
    walk_scoped(
        source,
        tree,
        path,
        files,
        config,
        interner,
        WalkScope::FileOnly,
    )
}

/// Does this path name a header, judged by its extension?
///
/// A guess, and deliberately a conservative one: getting it wrong for a `.hpp` costs a less confident but not
/// incorrect analysis, while inferring *from content* — "it has no `main`, so it must be a header" — would be
/// wrong in a way that is hard to notice. An unrecognised extension is treated as a translation unit, because
/// that is the reading that commits to the least.
fn is_probably_a_header(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    // Case-insensitively, because `Foo.H` is a header on the case-insensitive filesystems where it can exist
    // at all, and the cost of matching is nothing.
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "h" | "hh" | "hpp" | "hxx" | "h++" | "inc" | "ipp" | "tpp" | "tcc" | "inl"
    )
}

#[allow(clippy::too_many_arguments)]
fn visit<F: FileProvider>(
    source: &str,
    tree: &CppSyntaxTree,
    file: FileId,
    inherited: &Marked,
    graph: &mut FileGraph,
    state: &mut WalkState,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
    depth: usize,
) -> Marked {
    let _ = source;
    let root = tree.get_red_root();
    let preprocessing = crate::preprocess::preprocess(&root);
    let guard_analysis = analyse_guards(&preprocessing, &root);

    // What this file was reached with, recorded before anything in it changes the picture.
    let mut macros = inherited.clone();
    let inherited_names = macros.defined_names();

    let path = interner
        .path(file)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    record(
        graph,
        file,
        FileEntry {
            path: path.clone(),
            guard: guard_analysis.guard.clone(),
            defines: guard_analysis.defines.clone(),
            depth,
            macros: inherited_names,
            missing_context: state.missing_context(),
        },
    );

    state.visited.insert(file);
    state.on_path.push(file);

    let resolver = IncludeResolver::new(files, config);
    let including = parent_normalized(&path, files.is_case_insensitive());

    // The directives are walked **in order**, because the order is the whole point: a `#define` is in force
    // for the includes after it and not for the ones before, which is what makes two includes of one header
    // see two different macro tables. Walking only the includes would lose that.
    //
    // The macro state is snapshotted before each directive, because a condition has to be decided against the
    // macros in force where it was *written* — see [`directive_visibilities`].
    //
    // Snapshotting first and deciding afterwards is what makes the two uses of the state consistent: the
    // visibility of a directive is decided against the states of the directives *before* it, and this file's
    // own walk then applies the same verdict to decide whether to apply the directive at all.
    let mut states: Vec<Marked> = Vec::with_capacity(preprocessing.directives.len());

    for _ in &preprocessing.directives {
        states.push(macros.clone());
    }

    let visibilities = directive_visibilities(&states, &preprocessing.directives);

    for (index, spanned) in preprocessing.directives.iter().enumerate() {
        // A define inside a region that is definitely not compiled never takes effect. Applying it anyway
        // would put a macro in force that a compiler never defines — the failure that makes a consumer believe
        // a branch is live when it is not. An undecidable region is applied, because a macro that *might* be
        // defined is closer to what a compiler sees than one that is certainly not.
        if visibilities[index] == Some(false) {
            continue;
        }

        match &spanned.directive {
            Directive::Define(define) => {
                if let Some(definition) = &define.macro_def {
                    macros.define(definition.clone());
                }
            }
            Directive::Undef { name: Some(name) } => macros.undefine(name),
            _ => {}
        }

        let Some(include) = spanned.directive.as_include() else {
            continue;
        };

        // `#include_next` searches from where *this* file was found, and the walk does not record that per
        // file — a header reached by two routes has two answers. Passing `None` makes `#include_next`
        // behave as an ordinary include: wrong for a wrapper header, right for every other use. Written
        // down rather than left implicit, because it is the one part of resolution the walk cannot do.
        let resolution = resolver.resolve(include, &including, None, interner);

        match resolution {
            Resolution::Unresolved(unresolved) => graph.unresolved.push(UnresolvedInclude {
                from: file,
                name: unresolved.name,
                searched: unresolved.searched,
                range: spanned.range,
            }),
            Resolution::Resolved(resolved) => {
                let visit = decide_visit(resolved.file, graph, state, depth);

                graph.edges.push(Edge {
                    from: file,
                    to: resolved.file,
                    name: include.target.clone(),
                    is_angle: include.form == IncludeForm::Angle,
                    found_in: resolved.found_in,
                    visit,
                    range: spanned.range,
                });

                if visit == Visit::Analysed {
                    // The snapshot is of *this* moment: the macros in force here, at this include, which is
                    // what the included file's conditions are decided by.
                    let reached_with = macros.clone();
                    let after = follow(
                        resolved.file,
                        &resolved.path,
                        &reached_with,
                        graph,
                        state,
                        files,
                        config,
                        interner,
                        depth + 1,
                    );

                    // Textual inclusion: whatever the included file defined is in force from here on. A
                    // guarded file that was skipped contributes nothing new — it was read earlier and its
                    // definitions are already in `macros` — which is why a skipped visit needs no case of
                    // its own.
                    if let Some(after) = after {
                        macros = after;
                    }
                }
            }
        }
    }

    // What this file leaves behind, for its includer: everything in force at its end. The result — not the
    // *difference* — because the includer replaces its state with it, and a file that undefined something
    // must be able to remove it again.
    //
    // This is also what makes the chain correct rather than merely plausible. Passing the ancestor's final
    // state down instead of this one is the obvious mistake, and it fails visibly: a header's own guard is
    // already in force by the time its includer reaches the next include, so a second header is treated as
    // having been seen and its body is skipped.
    state.on_path.pop();
    macros
}

/// The visibility of every directive, by position: `Some(true)` for one that a compiler compiles,
/// `Some(false)` for one inside a region it does not, and `None` for one that cannot be decided.
///
/// One entry per directive in the file, in file order and index-aligned with it, so a caller can ask about any
/// of them — including the ones that are not conditions at all, which is what the walk needs: a `#define` or an
/// `#include` is skipped exactly when it sits inside a region that is not compiled.
///
/// # Why each open region remembers where its arms start
///
/// Asking "did some arm of this region hold" is not the same question as "is the code here inside the arm that
/// is in force", and the two differ wherever a region has more than one. Under the first reading
/// `#if 1 ... #else ... #endif` is compiled throughout, so the `#else` arm's `#define`s are applied although a
/// compiler never reads them. The region therefore keeps its arms' *starting positions* as well as their
/// verdicts, and the code at a position belongs to the last arm that started at or before it.
///
/// Positions rather than indices-within-the-region, because the question is about the file: an arm's index
/// within its region says nothing about where in the file it is.
///
/// # Why the verdicts are decided against a snapshot
///
/// Each arm's condition is decided against the macros in force **where it was written** — `states[position]`,
/// captured by the walk as it passed. Asking at the end gives the opposite answer for the one pattern that
/// matters most:
///
/// ```text
/// #ifndef GUARDED_H     <- true here: nothing has defined it yet
/// #define GUARDED_H     <- and now it is defined
/// ...                   <- everything from here reads as inside a condition that is now false
/// #endif
/// ```
///
/// Decide the guard from the final state and every guarded header's body looks disabled — its includes
/// included, so they are never followed.
///
/// # Why `#endif` is the one directive that changes nothing
///
/// Every directive's visibility is decided by the regions open **before** it, and `#endif` is no exception
/// however strange that reads: the `#endif` line is inside the region it closes, and a region that is disabled
/// makes its own `#endif` part of the disabled text. Applying that uniformly is what lets this be one loop
/// with no special case — the region is popped as the `#endif` is *passed*, which is what puts the directive
/// after it outside.
fn directive_visibilities(states: &[Marked], directives: &[SpannedDirective]) -> Vec<Option<bool>> {
    let mut open: Vec<OpenRegion> = Vec::new();
    let mut visibilities: Vec<Option<bool>> = Vec::with_capacity(directives.len());

    for (position, spanned) in directives.iter().enumerate() {
        // At a directive that is itself a condition, the region's own verdict is about the arm it opens: an
        // `#else` sits *inside* the region, in the arm it introduces. `inside_active_arm` places it there
        // because it counts arms starting at or before the position, and this arm starts here.
        visibilities.push(inside_active_arm(&open, position));

        // Every kind of condition is resolved here, `#endif` included, so that one branch of the match cannot
        // advance the state differently from another.
        let Some(branch) = branches_at(spanned) else {
            continue;
        };

        // The macros in force where this branch was written, which is what its expression is about.
        let state = states.get(position).unwrap_or_else(|| &states[0]);
        let holds = branch.decide(state);

        match branch.kind {
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                open.push(OpenRegion {
                    starts: vec![position],
                    holds: vec![holds],
                    active: if holds == Some(true) { Some(0) } else { None },
                })
            }
            DirectiveKind::Elif | DirectiveKind::Else => {
                if let Some(region) = open.last_mut() {
                    // An arm is eligible only when **every** arm before it is false. An `#else` tests nothing,
                    // so its own verdict says nothing about whether it is the arm in force — letting it
                    // activate itself from that verdict is what makes both halves of an `#if`/`#else` look
                    // compiled at once.
                    let eligible = region.holds.iter().all(|holds| *holds == Some(false));

                    region.starts.push(position);
                    region.holds.push(holds);
                    let this = region.holds.len() - 1;

                    if eligible && holds != Some(false) {
                        region.active = Some(this);
                    }
                }
            }
            DirectiveKind::Endif => {
                open.pop();
            }
            _ => {}
        }
    }

    visibilities
}

/// The conditional branch a directive introduces, if it introduces one.
fn branches_at(spanned: &SpannedDirective) -> Option<BranchAt> {
    let kind = spanned.directive.kind();
    if !(kind.opens_a_condition() || kind.closes_a_condition()) {
        return None;
    }

    let (tokens, name) = match &spanned.directive {
        Directive::Conditional { condition, .. } => (condition.clone(), None),
        Directive::Ifdef { name, .. } => (Vec::new(), Some(name.clone())),
        _ => (Vec::new(), None),
    };

    Some(BranchAt { kind, tokens, name })
}

/// Is the directive at `position` inside the arm in force of every open region?
///
/// `None` when some region could still take a later arm, which is the honest answer for a directive under an
/// `#if` on a macro the walk cannot value: it is compiled under some configurations and not others. Only a
/// region whose arms are all definitely false is a confident `Some(false)`.
fn inside_active_arm(open: &[OpenRegion], position: usize) -> Option<bool> {
    let mut undecided = false;

    for region in open {
        // The arm this directive falls in is the last one that started at or before it.
        let Some(current) = region.starts.iter().rposition(|start| *start <= position) else {
            continue;
        };

        match region.active {
            // Inside the arm in force: this region is no obstacle.
            Some(active) if active == current => continue,
            // Inside an arm that is definitely not the one in force — either a later one, or any arm of a
            // region where nothing holds at all.
            Some(_) => return Some(false),
            // No arm holds yet. An undecidable condition could still take a later arm, so this is not a
            // confident "no".
            None => {
                if region.holds.iter().all(|holds| *holds == Some(false)) {
                    return Some(false);
                }
                undecided = true;
            }
        }
    }

    if undecided { None } else { Some(true) }
}

/// A conditional region the walk is inside, with the arm that is in force.
#[derive(Debug)]
struct OpenRegion {
    /// Where each arm starts, by directive position.
    starts: Vec<usize>,
    /// Each arm's own verdict, in the same order: `Some(true)` when its condition held where it was written.
    holds: Vec<Option<bool>>,
    /// Index into the two above of the arm in force, or `None` when none holds yet.
    active: Option<usize>,
}

/// A conditional branch, remembered with what it tests.
///
/// The guard layer's [`crate::guard::Branch`] holds tokens and answers `holds` against a table; this holds the
/// same information in a form that can be decided later, against a *state* that is captured separately. The
/// split exists because the two are needed at different times: which branches exist is known from a file's
/// text, and what they mean is known only during the walk.
#[derive(Debug, Clone)]
pub struct BranchAt {
    pub kind: DirectiveKind,
    /// The expression's tokens, for `#if` and `#elif`.
    pub tokens: Vec<crate::token::Token>,
    /// The name tested, for `#ifdef` and `#ifndef`.
    pub name: Option<Box<str>>,
}

impl BranchAt {
    /// Does this branch's own expression hold under `state`?
    ///
    /// `None` when it cannot be decided, which is what makes the enclosing region `Unknown` rather than
    /// inactive. An `#else` has no expression and always holds: whether the region takes it is a question
    /// about the branches before it, which the region in force answers.
    ///
    /// `#ifdef` and `#ifndef` are where [`Marked::is_unknown`] earns its keep. A name the state does not
    /// mention is normally undefined, so `#ifdef` is false. In a header analysed on its own it may well have
    /// been defined by the includer, and answering `false` there would call the guarded code dead — the
    /// answer that makes a consumer report errors in code that compiles. Answering `None` instead says the
    /// only thing that is certainly true: this configuration does not decide it.
    pub fn decide(&self, state: &Marked) -> Option<bool> {
        match self.kind {
            DirectiveKind::If | DirectiveKind::Elif => {
                crate::condition::evaluate(&self.tokens, state).is_true()
            }
            DirectiveKind::Ifdef => self.named_status(state),
            DirectiveKind::Ifndef => self.named_status(state).map(|defined| !defined),
            DirectiveKind::Else => Some(true),
            _ => None,
        }
    }

    /// Whether the `#ifdef`/`#ifndef` name is defined, or `None` when this state cannot say.
    ///
    /// A directive that names nothing — `#ifdef` with no operand, which malformed source produces — reports
    /// the name as undefined rather than as unknown: there is nothing to be uncertain about.
    fn named_status(&self, state: &Marked) -> Option<bool> {
        let name = self.name.as_deref()?;

        if state.is_unknown(name) {
            return None;
        }

        Some(state.is_defined(name))
    }
}

/// What a visit to `file` should do, given where the walk has been.
fn decide_visit(file: FileId, graph: &FileGraph, state: &WalkState, depth: usize) -> Visit {
    // Asked for one file only. The include is still *recorded* — it is a fact about the file that was read,
    // and a consumer wants it — but reading the other end was not part of the question, so the reason says
    // that rather than pretending to be one of the facts about the code.
    if !state.follows_includes {
        return Visit::Skipped(SkipReason::OutOfScope);
    }

    if state.on_path.contains(&file) {
        return Visit::Skipped(SkipReason::Cycle);
    }

    if depth + 1 > MAX_INCLUDE_DEPTH {
        return Visit::Skipped(SkipReason::TooDeep);
    }

    // A guarded file already read is what the guard is for. An *unguarded* one is read again, because a
    // compiler reads it again — and a consumer that did not would miss the duplicate definitions that are
    // the reason guards get written in the first place.
    let guarded = graph
        .entry(file)
        .is_some_and(|entry| entry.guard.is_guarded());

    if guarded && state.visited.contains(&file) {
        return Visit::Skipped(SkipReason::AlreadyVisited);
    }

    Visit::Analysed
}

/// Read a file and visit it, returning the macros it left in force.
///
/// `None` when the file could not be read, which is not the same as "it left nothing behind": an unreadable
/// file is one whose contents are unknown, so its includer's macro state is left as it was rather than
/// replaced by an empty one.
#[allow(clippy::too_many_arguments)]
fn follow<F: FileProvider>(
    file: FileId,
    path: &Path,
    inherited: &Marked,
    graph: &mut FileGraph,
    state: &mut WalkState,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
    depth: usize,
) -> Option<Marked> {
    // A file that resolved but cannot be read is a file that exists to `exists` and not to `read`: a
    // directory, a permission problem, or a buffer closed between the two calls. Recorded so that a consumer
    // asking about it does not get `None` and conclude it was never reached.
    let Some(source) = files.read(path) else {
        record(
            graph,
            file,
            FileEntry {
                path: path.to_path_buf(),
                guard: Guard::None,
                defines: Vec::new(),
                depth,
                macros: inherited.defined_names(),
                // An unreadable file is not a file whose context is *known*: whatever it might have defined is
                // unknown along with the rest of it.
                missing_context: state.missing_context(),
            },
        );
        return None;
    };

    graph.analysed.push(file);
    let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());

    Some(visit(
        &source, &tree, file, inherited, graph, state, files, config, interner, depth,
    ))
}

/// Store an entry at the id's index, growing the table with placeholders for ids in between.
fn record(graph: &mut FileGraph, file: FileId, entry: FileEntry) {
    while graph.files.len() <= file.index() {
        graph.files.push(FileEntry {
            path: PathBuf::new(),
            guard: Guard::None,
            defines: Vec::new(),
            depth: usize::MAX,
            macros: Vec::new(),
            // A placeholder is not a file, so there is nothing here to have context about. `false` keeps a
            // consumer from reading `context_is_complete` as a statement about a file this walk never reached.
            missing_context: false,
        });
    }

    graph.files[file.index()] = entry;
}

/// The macro names in force at a point in a walk.
///
/// Names, not definitions, and deliberately so. Two questions need answering at this stage — "is `FOO`
/// defined" for an `#ifdef`, and "start this file off with these names" for the chain — and both are about
/// names. A macro's *value* matters once expansion runs, and that is [`crate::expand::expand`]'s job with a real
/// [`crate::macros::MacroTable`]; carrying values here would duplicate the table without the ordering rules
/// that make it correct.
///
/// The command line is the exception: `-DFOO` and `-DFOO=1` are parsed into definitions rather than names,
/// because that is where they come from and because dropping them would make every `#ifdef FOO` in a
/// configured project answer the wrong way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marked {
    /// Bindings in order, each name either present or explicitly undefined. Later binds shadow earlier ones,
    /// which is what makes `#undef` and redefinition work without deleting history.
    bindings: Vec<(Box<str>, bool)>,
    /// Definitions from the command line, kept so that a lookup can answer about a *value* as well as a name.
    command_line: Vec<crate::macros::MacroDef>,
    /// Whether macros this state does not mention might still have been in force.
    ///
    /// A state cannot tell "nothing defined this" apart from "this never saw the file that did", and for a
    /// header those are different answers. With this set, a name with no binding is **unknown** rather than
    /// undefined — which is what keeps an `#ifdef` from a header's includer going the wrong way.
    incomplete: bool,
}

impl crate::condition::MacroValues for Marked {
    /// The definition behind a name, for evaluating a condition.
    ///
    /// Only names that came from the command line have one here: `-DFOO=2` is a definition this layer read, so
    /// `#if FOO == 2` can be decided. A name a `#define` introduced in a file the walk has passed through has
    /// no body in this state, so `#ifdef FOO` is answered correctly by [`Marked::is_defined`] while a *value*
    /// question answers `Unknown` — which [`crate::guard`] already reports as undecidable rather than false.
    ///
    /// That gap is deliberate and bounded: carrying every body would mean duplicating the macro table without
    /// its ordering rules, and what needs deciding at this stage is whether a region is `#if 0`d out.
    fn lookup(&self, name: &str) -> Option<&crate::macros::MacroDef> {
        self.get(name)
    }
}

impl Marked {
    /// A state that knows only what it has seen, for a file whose includers are not part of the analysis.
    ///
    /// See [`FileEntry::missing_context`] for why this is the honest reading rather than the pessimistic one.
    pub fn incomplete(mut self) -> Self {
        self.incomplete = true;
        self
    }

    /// Is `name`'s status unknown — neither defined nor definitely not?
    ///
    /// True only when the state is [`Marked::incomplete`] and nothing in it mentions the name either way. A
    /// name that was explicitly `#undef`ed is *not* unknown: the file that undefined it is one this state has
    /// seen, so the answer is a definite no.
    pub fn is_unknown(&self, name: &str) -> bool {
        self.incomplete && !self.bindings.iter().any(|(bound, _)| &**bound == name)
    }

    /// The state a translation unit starts in: the compiler's own definitions.
    pub fn from_config(config: &CompilerConfig) -> Self {
        let mut marked = Marked::default();

        for definition in &config.defines {
            // `-DFOO` is `FOO` with the body `1`; `-DFOO=bar` is `FOO` with the body `bar`. Both are
            // re-parsed through the ordinary `#define` reader so that the two spellings cannot drift apart.
            let source = match &definition.value {
                Some(value) => format!("#define {} {}\n", definition.name, value),
                None => format!("#define {} 1\n", definition.name),
            };

            if let Some(parsed) = parse_define_source(&source) {
                marked.command_line.push(parsed.clone());
                marked.bindings.push((parsed.name.clone(), true));
            }
        }

        for name in &config.undefines {
            marked.bindings.push((name.clone(), false));
        }

        marked
    }

    pub fn define(&mut self, definition: crate::macros::MacroDef) {
        self.bindings.push((definition.name.clone(), true));
        self.command_line
            .retain(|existing| existing.name != definition.name);
        self.command_line.push(definition);
    }

    pub fn define_name(&mut self, name: &str) {
        self.bindings.push((name.into(), true));
    }
    pub fn undefine(&mut self, name: &str) {
        self.bindings.push((name.into(), false));
        self.command_line.retain(|existing| &*existing.name != name);
    }

    pub fn is_defined(&self, name: &str) -> bool {
        self.bindings
            .iter()
            .rev()
            .find(|(bound, _)| &**bound == name)
            .is_some_and(|(_, defined)| *defined)
    }

    pub fn get(&self, name: &str) -> Option<&crate::macros::MacroDef> {
        if !self.is_defined(name) {
            return None;
        }

        self.command_line
            .iter()
            .rev()
            .find(|definition| &*definition.name == name)
    }

    /// Every name currently defined, sorted and deduplicated — a name that was undefined must not appear,
    /// and one that was redefined must not appear twice.
    ///
    /// The shadowing has to be resolved **before** the sort, not after: a name bound twice appears as two
    /// entries in `bindings`, and only the last one is in force. Filtering for `defined` first and deduping
    /// afterwards keeps the *earlier* binding and throws the later one away, which is exactly backwards — it
    /// makes `#undef FOO` invisible and a definition that has been removed look as if it is still in force.
    pub fn defined_names(&self) -> Vec<Box<str>> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut names: Vec<Box<str>> = Vec::new();

        for (name, defined) in self.bindings.iter().rev() {
            if !seen.insert(name) {
                continue;
            }

            if *defined {
                names.push(name.clone());
            }
        }

        names.sort_unstable();
        names
    }

    pub fn len(&self) -> usize {
        self.defined_names().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Read one `#define` from a source string, the way a directive's tokens are read.
fn parse_define_source(source: &str) -> Option<crate::macros::MacroDef> {
    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    let preprocessing = crate::preprocess::preprocess(&tree.get_red_root());
    preprocessing.macros.iter().next().cloned()
}

#[cfg(test)]
mod tests {
    use super::{CompilerConfig, Marked, Path, is_probably_a_header};

    /// A name bound twice is in force according to its **last** binding, and `defined_names` has to resolve
    /// that before it dedupes.
    ///
    /// Getting the order wrong is a silently wrong answer rather than a crash: excluding the later binding and
    /// keeping the earlier one makes `#undef FOO` invisible, so a macro that has been removed still looks
    /// defined to everything downstream — including an `#ifdef` in a header the walk has not reached yet, and
    /// so including the macros that header contributes.
    #[test]
    fn a_later_binding_shadows_an_earlier_one() {
        let mut marked = Marked::default();
        marked.define_name("REDEFINED");
        marked.define_name("REMOVED");
        marked.define_name("REDEFINED");
        marked.undefine("REMOVED");

        assert_eq!(
            marked
                .defined_names()
                .iter()
                .map(|n| &**n)
                .collect::<Vec<_>>(),
            vec!["REDEFINED"],
            "the later bindings are the ones in force"
        );

        // A name defined, removed, and defined again is defined: the resurrection is a later binding too.
        let mut marked = Marked::default();
        marked.define_name("BACK");
        marked.undefine("BACK");
        marked.define_name("BACK");

        assert_eq!(
            marked
                .defined_names()
                .iter()
                .map(|n| &**n)
                .collect::<Vec<_>>(),
            vec!["BACK"]
        );
    }

    /// A name defined twice is reported once, and the result is sorted so that two walks over the same file
    /// cannot produce two different answers to "what is in force here".
    #[test]
    fn defined_names_is_sorted_and_deduplicated() {
        let mut marked = Marked::default();
        marked.define_name("ZED");
        marked.define_name("ALPHA");
        marked.define_name("ZED");
        marked.define_name("MID");

        assert_eq!(
            marked
                .defined_names()
                .iter()
                .map(|n| &**n)
                .collect::<Vec<_>>(),
            vec!["ALPHA", "MID", "ZED"]
        );
        assert_eq!(marked.len(), 3);
    }

    /// An empty state defines nothing, which is what makes `is_empty` usable as "no macros are in force".
    #[test]
    fn an_empty_state_has_no_names() {
        let marked = Marked::default();
        assert!(marked.is_empty());
        assert!(marked.defined_names().is_empty());
        assert!(!marked.is_defined("ANYTHING"));
    }

    /// In a complete state, a name nothing mentions is **undefined** — not unknown.
    ///
    /// This is the answer that decides `#if 0`-style pruning for a translation unit, and weakening it to
    /// "unknown" would make every `#ifdef` in a configured project undecidable — which reports dead code as
    /// live, and is precisely the false positive the whole design is arranged to avoid.
    #[test]
    fn a_complete_state_reports_an_unmentioned_name_as_undefined() {
        let marked = Marked::from_config(&CompilerConfig::new());

        assert!(!marked.is_unknown("ANYTHING"));
        assert!(!marked.is_defined("ANYTHING"));
    }

    /// An incomplete state reports a name nothing mentions as **unknown**, because the file that would have
    /// defined it may not be part of the analysis.
    ///
    /// A name the state *has* seen is not unknown, and the distinction is what makes `#undef` still mean
    /// something in a header: the header undefined it itself, so the answer is a definite no rather than an
    /// open question.
    #[test]
    fn an_incomplete_state_reports_an_unmentioned_name_as_unknown() {
        let marked = Marked::default().incomplete();

        assert!(marked.is_unknown("FROM_THE_INCLUDER"));
        assert!(!marked.is_defined("FROM_THE_INCLUDER"));

        let mut seen = Marked::default().incomplete();
        seen.define_name("DEFINED_HERE");
        seen.undefine("UNDEFINED_HERE");

        assert!(!seen.is_unknown("DEFINED_HERE"));
        assert!(!seen.is_unknown("UNDEFINED_HERE"));
        assert!(seen.is_defined("DEFINED_HERE"));
        assert!(!seen.is_defined("UNDEFINED_HERE"));
    }

    /// The header guess is made from the extension, and an unknown extension is treated as a translation
    /// unit: that is the reading which commits to the least, since it claims no caveat it cannot support.
    #[test]
    fn a_header_is_recognised_by_its_extension() {
        for header in [
            "a.h",
            "a.hh",
            "a.hpp",
            "a.hxx",
            "a.h++",
            "a.inc",
            "a.ipp",
            "a.tpp",
            "a.tcc",
            "a.inl",
            "A.H",
            "dir/sub/a.hpp",
        ] {
            assert!(
                is_probably_a_header(Path::new(header)),
                "{header} is a header"
            );
        }

        for source in [
            "a.cpp",
            "a.cc",
            "a.cxx",
            "a.c",
            "a.c++",
            "a.m",
            "a.mm",
            "a",
            "a.hpp.cpp",
            "a.txt",
            "Makefile",
        ] {
            assert!(
                !is_probably_a_header(Path::new(source)),
                "{source} is not a header"
            );
        }
    }
}
