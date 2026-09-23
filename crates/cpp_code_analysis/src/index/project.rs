//! Many files at once: the summaries, the edges between them, and the queries that need both.
//!
//! [`crate::index`] turns one file into a [`FileSummary`]. This module is what holds a project's worth of them
//! and answers the questions that cross a file boundary — which is the whole reason an index exists, and the
//! layer `docs/index-design.md` calls `resolve`.
//!
//! # Why the reverse map is built here and not stored
//!
//! A summary stores the includes its own file *writes* — direct edges, one per `#include`. The map from a header
//! back to the files that include it is therefore **derived**, and it is derived here, in memory, from the
//! summaries as they are loaded. Writing it to disk would be storing a conclusion: it is a fact about the graph
//! rather than about any file, so it would have no single file to go stale with, and the first inconsistency
//! between it and the summaries would be invisible. Rebuilding it costs one pass over the includes, which is
//! what the summaries are for.
//!
//! # What visible means here
//!
//! ```text
//! a.h  declares Widget
//! b.h  #include "a.h"
//! c.cpp #include "b.h"      -> Widget is visible in c.cpp
//! d.cpp (nothing)           -> Widget is not visible in d.cpp, even though it is in the project
//! ```
//!
//! So a name is visible in a file when the file **transitively includes** the file that declares it. That is a
//! graph reachability question, and it is answered without re-reading anything: the edges are in the summaries.
//!
//! # The one case this cannot decide, and what it says instead
//!
//! An `#include` written inside an `#if` is a fact about the text, not about a compilation: whether the compiler
//! took that branch depends on macros the index does not have. So a declaration reached only through a
//! **guarded** include is reported as [`Known::Unknown`] rather than as visible or invisible — the same rule the
//! rest of the crate follows, and the reason [`IncludeFact`](crate::summary::IncludeFact) carries a guard at all.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::include::paths::normalize_path;
use crate::summary::{DeclFact, FactGuard, FileSummary};
use crate::symbol::{Known, UnknownReason};

/// How many files a visibility walk will cross before giving up.
///
/// A backstop rather than a policy: real include graphs are shallow (the walker's own limit is
/// [`crate::MAX_INCLUDE_DEPTH`]), and a corpus that exceeds this is one whose summaries disagree with its
/// includes — which the visited set already handles. The number is here so that the *round* count of a
/// pathological graph cannot become an unbounded amount of work on a keystroke.
const MAX_VISIBILITY_DEPTH: usize = 128;

/// Which declaration something refers to, using **both** layers.
///
/// The entry point a feature should call, and the reason it exists rather than each caller composing the two:
/// C++ resolves a name in a fixed order, and getting that order wrong is a jump to the wrong file rather than an
/// error. The order is:
///
/// ```text
/// 1. this file's scopes        — a local shadows a header's declaration, always
/// 2. this file's own top-level — a declaration written here is not the header's
/// 3. the headers it includes   — reached through the include graph
/// ```
///
/// # What it takes, and why each half is a reference
///
/// `scopes` and `root` are the file's own analysis, which [`crate::build_scopes`] and the parser produce — the
/// caller has them because it just parsed the file. The index holds the *other* files. Neither is derivable from
/// the other: the index has no scopes for the open buffer, and the scopes know nothing outside it.
///
/// # The two reasons that reach step 3, and the one that does not
///
/// [`UnknownReason::NotDeclaredHere`] means the name is not in this file, which is exactly what step 3 is for.
/// [`UnknownReason::Ambiguous`] does **not** mean that: the name *is* here, more than once, and looking in the
/// headers would be answering a different question. So it stops.
///
/// Everything else stops too — a name that could not be read, a conditional the analysis cannot evaluate — for
/// the same reason: the failure is not "not here".
pub fn definition_across_files(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    match crate::sema::resolve::definition_at(scopes, root, offset) {
        Known::Yes(binding) => return Known::Yes(ProjectDefinition::from_binding(path, binding)),
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => {
            // The single-file layer has already established the spelling, so the project layer is asked about
            // exactly that name rather than re-reading the cursor.
            return index.definition(&name, path);
        }
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {}
    }

    // `No` from the single-file layer means the offset is not in a name the scopes could place at all — on
    // punctuation, or past the end — so there is nothing to look up anywhere.
    Known::Unknown(UnknownReason::UnparsableName)
}

/// A project's summaries, and the queries that need more than one of them.
///
/// Built incrementally: [`ProjectIndex::insert`] takes a summary that some other layer produced, which is what
/// keeps this type free of any opinion about parsing, caching or the filesystem.
#[derive(Debug, Default)]
pub struct ProjectIndex {
    /// The summaries, by normalized path.
    summaries: HashMap<String, FileSummary>,
    /// The paths in insertion order, so that a query over all of them is deterministic rather than
    /// `HashMap`-ordered. A definition jump that returned a different file on each run would be a bug that only
    /// shows up in a test that runs twice.
    order: Vec<String>,
    /// For each file, the files that include it. Derived from the summaries; see the module documentation.
    included_by: HashMap<String, BTreeSet<String>>,
}

impl ProjectIndex {
    pub fn new() -> Self {
        ProjectIndex::default()
    }

    /// Add or replace one file's summary.
    ///
    /// The reverse edges are updated rather than rebuilt: an edit to one file changes only its own out-edges,
    /// and rebuilding the whole map on every keystroke is the cost the per-file design exists to avoid.
    pub fn insert(&mut self, summary: FileSummary) {
        let path = summary.path.clone();
        self.insert_at(&path, summary);
    }

    /// [`ProjectIndex::insert`] for a summary that was **read from the cache**.
    ///
    /// `path` is the file it is being filed under, and it has to be passed rather than taken from the summary
    /// because the two can legitimately differ: a cache entry is keyed on a file's *contents* and the directory it
    /// was compiled in, so two files with identical text side by side share an entry, and the entry's own `path`
    /// records whichever of them was written first. Filing the second under the first's name makes every fact in
    /// it point at the wrong file, and a definition jump into a file the user never mentioned.
    ///
    /// The facts themselves are right either way: a summary's contents are a function of the text *and its
    /// directory* — which is exactly what the key names, and the reason the directory is part of it. Two files
    /// whose keys match have the same declarations, at the same offsets, with the same ranges, *and* the same
    /// resolved includes.
    pub fn insert_at(&mut self, path: &Path, summary: FileSummary) {
        let mut summary = summary;
        summary.path = path.to_path_buf();

        let path = normalize(path);

        // Remove the edges the previous version of this file contributed, so that a deleted `#include` stops
        // making its target reachable. Without this, an edge would outlive the line that wrote it.
        if let Some(previous) = self.summaries.get(&path) {
            for target in include_targets(previous) {
                if let Some(includers) = self.included_by.get_mut(&target) {
                    includers.remove(&path);
                }
            }
        } else {
            self.order.push(path.clone());
        }

        for target in include_targets(&summary) {
            self.included_by
                .entry(target)
                .or_default()
                .insert(path.clone());
        }

        self.summaries.insert(path, summary);
    }

    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    /// The summary of the file at `path`, if it has been indexed.
    pub fn summary(&self, path: &Path) -> Option<&FileSummary> {
        self.summaries.get(&normalize(path))
    }

    /// Every summary, in insertion order.
    pub fn summaries(&self) -> impl Iterator<Item = &FileSummary> {
        self.order.iter().filter_map(|path| self.summaries.get(path))
    }

    /// The files that include `path`, directly.
    pub fn includers_of(&self, path: &Path) -> Vec<PathBuf> {
        self.included_by
            .get(&normalize(path))
            .map(|includers| includers.iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// The files in which `name` is visible, in insertion order.
    ///
    /// `name` is matched against a declaration's **qualified** name first — `ns::Widget` — and against its bare
    /// name only as a fallback, because a qualified spelling is a much stronger claim than a name that happens to
    /// appear in some scope. A caller that gets one answer from the qualified match should prefer it to any
    /// number from the bare one.
    pub fn files_declaring(&self, name: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        let mut found = Vec::new();

        for summary in self.summaries() {
            let Some(visibility) = self.visibility_of(&summary.path, visible_from) else {
                continue;
            };

            for fact in summary.declarations.iter().filter(|fact| matches(fact, name)) {
                found.push(VisibleDeclaration {
                    file: summary.path.clone(),
                    fact,
                    visibility,
                });
            }
        }

        found
    }

    /// Which declaration a name written in `visible_from` refers to, across the project.
    ///
    /// The cross-file half of [`crate::sema::resolve::definition_at`], and the answer to the
    /// the single-file query returns when a name is somewhere else.
    ///
    /// # The four answers
    ///
    /// * `Yes` — exactly one visible declaration, unconditionally reachable.
    /// * `Unknown(NotDeclaredHere)` — nothing in the project declares it, or nothing that is visible. **Not**
    ///   `No`: the project's index is a subset of what a compiler would see (the standard library, a header
    ///   outside every include path), so "not here" and "nowhere" are still different claims.
    /// * `Unknown(Ambiguous)` — several declarations are visible and nothing chooses between them. Overloads, a
    ///   name declared in two headers the file includes, a bare name declared in two namespaces.
    /// * `Unknown(ConditionalCompilation)` — the only match is reached through an `#include` inside an `#if`, so
    ///   whether it is in scope depends on macros this layer does not have.
    pub fn definition(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinition> {
        let candidates = self.files_declaring(name, visible_from);

        if candidates.is_empty() {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        // A declaration the file itself writes wins over one it includes, and is the one a reader means: a
        // header's `Widget` and this file's `Widget` are different entities, and C++ resolves to the local one.
        let own = normalize(visible_from);
        if let Some(local) = candidates.iter().find(|found| normalize(&found.file) == own) {
            return Known::Yes(ProjectDefinition {
                file: local.file.clone(),
                fact: local.fact.clone(),
            });
        }

        // Prefer the unambiguous ones: a declaration reachable without any conditional include is visible
        // whatever the macros are, so it is a better answer than one that might not be there.
        let unconditional: Vec<&VisibleDeclaration<'_>> = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Unconditional)
            .collect();
        let guarded: Vec<&VisibleDeclaration<'_>> = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Conditional)
            .collect();

        if unconditional.len() == 1 {
            let found = unconditional[0];
            return Known::Yes(ProjectDefinition {
                file: found.file.clone(),
                fact: found.fact.clone(),
            });
        }
        if !unconditional.is_empty() {
            return Known::Unknown(UnknownReason::Ambiguous(Box::from(name)));
        }

        if guarded.is_empty() {
            // Reachable only through a missing file, which `visibility_of` reports as not visible — so this arm
            // is unreachable in practice and exists so that a future visibility answer has to be handled here
            // rather than silently falling through to `Yes`.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        Known::Unknown(UnknownReason::ConditionalCompilation)
    }

    /// How `from` reaches `target`, or `None` when it does not.
    fn visibility_of(&self, target: &Path, from: &Path) -> Option<IncludeVisibility> {
        let from = normalize(from);
        let target = normalize(target);

        if from == target {
            return Some(IncludeVisibility::Unconditional);
        }

        // Breadth-first from the querying file, carrying whether any step so far was conditional. A path that
        // exists in two forms — one conditional, one not — is reported as unconditional, because the
        // unconditional one is the one that is always there.
        let mut visited: HashSet<String> = HashSet::new();
        let mut pending: Vec<(String, IncludeVisibility, usize)> =
            vec![(from, IncludeVisibility::Unconditional, 0)];
        let mut best: Option<IncludeVisibility> = None;

        while let Some((current, so_far, depth)) = pending.pop() {
            if depth > MAX_VISIBILITY_DEPTH {
                continue;
            }

            let Some(summary) = self.summaries.get(&current) else {
                continue;
            };

            for include in &summary.includes {
                let Some(resolved) = &include.resolved else {
                    continue;
                };
                let next = normalize(resolved);

                let step = match include.guard {
                    FactGuard::Unconditional => so_far,
                    FactGuard::Region(_) => IncludeVisibility::Conditional,
                };

                if next == target {
                    match step {
                        IncludeVisibility::Unconditional => return Some(step),
                        IncludeVisibility::Conditional => best = Some(step),
                    }
                    continue;
                }

                if visited.insert(next.clone()) {
                    pending.push((next, step, depth + 1));
                }
            }
        }

        best
    }
}

/// A declaration found in another file, with how the file that asked reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleDeclaration<'a> {
    pub file: PathBuf,
    pub fact: &'a DeclFact,
    pub visibility: IncludeVisibility,
}

/// The answer to a cross-file definition question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinition {
    pub file: PathBuf,
    /// The declaration, cloned out of the index so that an answer does not borrow the project for as long as a
    /// consumer wants to hold it — a language server hands the location to a client and moves on.
    pub fact: DeclFact,
}

impl ProjectDefinition {
    /// The same shape, from a binding the file's own scopes produced.
    ///
    /// [`crate::sema::resolve::definition_at`] answers with a [`Binding`], which carries a `Name` rather than a plain
    /// string and no qualified scope — so the two answers are made to look alike here, in one place, rather than
    /// at every call site. The scope prefix is deliberately left empty: a binding found in *this* file's scopes
    /// is already the answer, and inventing a qualification for it would be inventing a second spelling of a
    /// declaration that has one.
    ///
    /// [`Binding`]: crate::Binding
    pub fn from_binding(path: &Path, binding: crate::Binding) -> Self {
        ProjectDefinition {
            file: path.to_path_buf(),
            fact: DeclFact {
                name: binding
                    .name
                    .identifier_text()
                    .unwrap_or_default()
                    .to_string(),
                scope: None,
                kind: crate::DeclKind::from_binding_kind(binding.kind),
                range: binding.range,
                name_range: binding.name_range,
                guard: FactGuard::Unconditional,
            },
        }
    }
}

/// Is the path to a declaration always taken, or only under conditions the index cannot evaluate?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeVisibility {
    /// Every `#include` on the path is outside any `#if`, so the declaration is in scope whatever the macros are.
    Unconditional,
    /// At least one `#include` on the path is inside an `#if` this layer cannot evaluate.
    Conditional,
}

/// Does this declaration answer to `name`?
///
/// The qualified name first, then the bare one. Both are needed and the order matters: `ns::Widget` written in
/// the query is a claim that the reader knows where the name lives, and honouring it must not be diluted by
/// every unrelated `Widget` elsewhere in the project.
fn matches(fact: &DeclFact, name: &str) -> bool {
    fact.qualified_name() == name || fact.name == name
}

/// The resolved include targets of a summary, as normalized path strings.
fn include_targets(summary: &FileSummary) -> Vec<String> {
    summary
        .includes
        .iter()
        .filter_map(|include| include.resolved.as_ref())
        .map(|path| normalize(path))
        .collect()
}

/// A path as the index keys on it.
fn normalize(path: &Path) -> String {
    // Case-insensitive on Windows, where two spellings of one path are one file. The rest of the crate reads the
    // same flag from the compiler configuration; here it is the platform, because the *index* has to agree with
    // the filesystem about identity and nothing else.
    normalize_path(path, cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::{IncludeVisibility, ProjectIndex};
    use crate::cache::SummaryKey;
    use crate::index::summarize;
    use crate::symbol::{Known, UnknownReason};
    use std::path::Path;

    fn index(files: &[(&str, &str)]) -> ProjectIndex {
        let mut index = ProjectIndex::new();

        for (path, source) in files {
            // The includes are resolved by hand here rather than through a `FileProvider`, because what these
            // tests are about is the graph that results — not the search that produced it.
            let mut summary = summarize(Path::new(path), source, SummaryKey::new(0, 0));
            for include in &mut summary.includes {
                include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
            }
            index.insert(summary);
        }

        index
    }

    #[test]
    fn a_declaration_in_an_included_header_is_found() {
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the class in the included header must be found: {found:?}");
        };

        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert_eq!(definition.fact.name, "Widget");
    }

    #[test]
    fn a_declaration_in_a_header_the_file_does_not_include_is_not_found() {
        // The distinction the whole visibility walk exists for: `Widget` is in the project, and not in scope
        // here. Answering `Yes` would be a jump to a declaration the file cannot compile against.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/other.cpp", "void g() { }\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/other.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "a name that is not in scope is not a definition: {found:?}"
        );
    }

    #[test]
    fn visibility_follows_a_chain_of_includes() {
        let index = index(&[
            ("/p/deep.h", "struct Deep { int x; };\n"),
            ("/p/middle.h", "#include \"deep.h\"\n"),
            ("/p/main.cpp", "#include \"middle.h\"\nvoid f() { Deep d; }\n"),
        ]);

        let found = index.definition("Deep", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("a transitively included declaration is visible: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/deep.h"));
    }

    #[test]
    fn the_files_own_declaration_wins_over_an_included_one() {
        // C++ resolves to the declaration in the file being compiled, and a jump that went into a header instead
        // would be to a different entity.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int from_header; };\n"),
            (
                "/p/main.cpp",
                "#include \"widget.h\"\nstruct Widget { int from_this_file; };\n",
            ),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the local declaration wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
    }

    #[test]
    fn a_name_declared_in_two_visible_headers_is_ambiguous_rather_than_guessed() {
        let index = index(&[
            ("/p/one.h", "int count;\n"),
            ("/p/two.h", "int count;\n"),
            (
                "/p/main.cpp",
                "#include \"one.h\"\n#include \"two.h\"\nvoid f() { count = 1; }\n",
            ),
        ]);

        let found = index.definition("count", Path::new("/p/main.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::Ambiguous(_))),
            "two declarations and nothing to choose between them: {found:?}"
        );
    }

    #[test]
    fn a_qualified_name_prefers_the_declaration_that_matches_it() {
        let index = index(&[
            ("/p/a.h", "namespace a {\n  struct Widget { int x; };\n}\n"),
            ("/p/b.h", "namespace b {\n  struct Widget { int y; };\n}\n"),
            (
                "/p/main.cpp",
                "#include \"a.h\"\n#include \"b.h\"\nvoid f() { a::Widget w; }\n",
            ),
        ]);

        let found = index.definition("a::Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the qualified name must select one of them: {found:?}");
        };

        assert_eq!(definition.file, Path::new("/p/a.h"));
        assert_eq!(definition.fact.qualified_name(), "a::Widget");
    }

    #[test]
    fn a_guarded_include_makes_the_answer_unknown() {
        // `#include` inside an `#if`: whether it is taken depends on macros the index does not have, so the
        // honest answer is that the name may or may not be in scope.
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            (
                "/p/main.cpp",
                "#if defined(USE_WIDGET)\n#include \"widget.h\"\n#endif\nvoid f() { Widget w; }\n",
            ),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));

        assert!(
            matches!(found, Known::Unknown(UnknownReason::ConditionalCompilation)),
            "a conditional include cannot be decided here: {found:?}"
        );
    }

    #[test]
    fn an_unconditional_include_beats_a_guarded_one() {
        // The same name reachable two ways: the unguarded path is always there, so it is the answer.
        let index = index(&[
            ("/p/real.h", "struct Widget { int size; };\n"),
            (
                "/p/main.cpp",
                "#include \"real.h\"\n#if defined(X)\n#include \"other.h\"\n#endif\nvoid f() { Widget w; }\n",
            ),
            ("/p/other.h", "struct Widget { int other; };\n"),
        ]);

        let found = index.definition("Widget", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("the unconditional path wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/real.h"));
    }

    #[test]
    fn a_file_that_includes_nothing_sees_only_itself() {
        let index = index(&[
            ("/p/a.cpp", "int count;\n"),
            ("/p/b.cpp", "int count;\n"),
        ]);

        for path in ["/p/a.cpp", "/p/b.cpp"] {
            let found = index.definition("count", Path::new(path));
            let Known::Yes(definition) = found else {
                panic!("{path} must find its own count: {found:?}");
            };
            assert_eq!(definition.file, Path::new(path));
        }
    }

    #[test]
    fn a_cycle_of_includes_does_not_hang_the_visibility_walk() {
        let index = index(&[
            ("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n"),
            ("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n"),
            ("/p/main.cpp", "#include \"a.h\"\nvoid f() { B b; }\n"),
        ]);

        let found = index.definition("B", Path::new("/p/main.cpp"));
        let Known::Yes(definition) = found else {
            panic!("a cycle must not stop a name being found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/b.h"));
    }

    #[test]
    fn the_reverse_edges_are_derived_from_the_summaries() {
        let index = index(&[
            ("/p/header.h", "int x;\n"),
            ("/p/one.cpp", "#include \"header.h\"\n"),
            ("/p/two.cpp", "#include \"header.h\"\n"),
        ]);

        let mut includers = index.includers_of(Path::new("/p/header.h"));
        includers.sort();
        assert_eq!(
            includers,
            [Path::new("/p/one.cpp"), Path::new("/p/two.cpp")],
            "an edit to the header invalidates exactly these"
        );
    }

    #[test]
    fn reindexing_a_file_removes_the_edges_its_old_text_had() {
        // An edge that outlived the `#include` that wrote it would keep a header reachable from a file that no
        // longer includes it — a stale conclusion with nothing on disk to contradict it.
        let mut index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n"),
        ]);

        assert!(index.includers_of(Path::new("/p/widget.h")).len() == 1);

        let mut edited = summarize(Path::new("/p/main.cpp"), "void f() { }\n", SummaryKey::new(1, 0));
        for include in &mut edited.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(edited);

        assert!(
            index.includers_of(Path::new("/p/widget.h")).is_empty(),
            "the edge must go with the line that wrote it"
        );
        assert!(
            matches!(
                index.definition("Widget", Path::new("/p/main.cpp")),
                Known::Unknown(_)
            ),
            "and the name is no longer visible there"
        );
    }

    #[test]
    fn visibility_is_reported_so_a_caller_can_downgrade() {
        let index = index(&[
            ("/p/widget.h", "struct Widget { int size; };\n"),
            ("/p/main.cpp", "#include \"widget.h\"\n"),
        ]);

        let found = index.files_declaring("Widget", Path::new("/p/main.cpp"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].visibility, IncludeVisibility::Unconditional);
    }

    /// The offset of the last `needle`, which is the cursor position in these fixtures.
    fn at(source: &str, needle: &str) -> usize {
        source
            .rfind(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
    }

    /// An index over the fixtures **plus** the analysed querying file, which is what
    /// [`super::definition_across_files`] needs: the index holds the other files, and the scope tree holds this
    /// one.
    fn analysed(
        files: &[(&str, &str)],
        from: &str,
        source: &str,
    ) -> (ProjectIndex, cpp_parser::CppSyntaxTree) {
        let mut index = index(files);

        let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "the fixture must parse cleanly: {source:?}"
        );

        // The file being queried is indexed too, and its summary must describe the same text whose tree is used
        // below — otherwise the two would disagree about offsets, and the tests would pass for the wrong reason.
        let mut summary = summarize(Path::new(from), source, SummaryKey::new(0, 0));
        for include in &mut summary.includes {
            include.resolved = Some(std::path::PathBuf::from(format!("/p/{}", include.spelling)));
        }
        index.insert(summary);

        (index, tree)
    }

    #[test]
    fn a_local_declaration_is_answered_without_looking_in_the_index() {
        // The first step of the resolution order, and the one that has to win: a local shadows a header's
        // declaration, so a jump that went into the header would be to a different entity.
        let source = "#include \"widget.h\"\nvoid f() {\n  int count = 0;\n  count = 1;\n}\n";
        let (index, tree) = analysed(&[("/p/widget.h", "int count;\n")], "/p/main.cpp", source);

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "count = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the local must win: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
        assert!(
            definition.fact.range.start_offset > at(source, "void f"),
            "the jump goes to the local, not to the header"
        );
    }

    #[test]
    fn a_name_only_a_header_declares_is_found_through_the_index() {
        let source = "#include \"widget.h\"\nvoid f() {\n  Widget w;\n}\n";
        let (index, tree) = analysed(
            &[("/p/widget.h", "struct Widget { int size; };\n")],
            "/p/main.cpp",
            source,
        );

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "Widget w;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the header's class must be found: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/widget.h"));
        assert_eq!(definition.fact.name, "Widget");
    }

    #[test]
    fn a_local_declaration_still_wins_when_the_header_also_has_the_name() {
        // `count` is declared both here and in the header. The answer is this file's, and it is reached without
        // the index — which is the resolution order, not an optimisation.
        let source = "#include \"widget.h\"\nint count = 0;\nvoid f() {\n  count = 1;\n}\n";
        let (index, tree) = analysed(&[("/p/widget.h", "int count = 7;\n")], "/p/main.cpp", source);

        let root = tree.get_red_root();
        let scopes = crate::build_scopes(&root);
        let found = super::definition_across_files(
            &index,
            &scopes,
            &root,
            Path::new("/p/main.cpp"),
            at(source, "count = 1;"),
        );

        let Known::Yes(definition) = found else {
            panic!("the file's own declaration wins: {found:?}");
        };
        assert_eq!(definition.file, Path::new("/p/main.cpp"));
    }
}